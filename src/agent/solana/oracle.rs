// This module is responsible for loading the current state of the
// on-chain Oracle program accounts from Solana.

use {
    self::subscriber::Subscriber,
    crate::agent::store::global,
    anyhow::{
        anyhow,
        Result,
    },
    pyth_sdk_solana::state::{
        load_mapping_account,
        load_price_account,
        load_product_account,
    },
    serde::Deserialize,
    slog::Logger,
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_sdk::{
        account::Account,
        commitment_config::{
            CommitmentConfig,
            CommitmentLevel,
        },
        pubkey::Pubkey,
    },
    std::{
        collections::HashMap,
        time::Duration,
    },
    tokio::{
        sync::mpsc,
        task::JoinHandle,
        time::Interval,
    },
};

#[derive(Default, Debug, Clone)]
pub struct Data {
    pub mapping_accounts: HashMap<Pubkey, MappingAccount>,
    pub product_accounts: HashMap<Pubkey, ProductAccount>,
    pub price_accounts:   HashMap<Pubkey, PriceAccount>,
}

pub type MappingAccount = pyth_sdk_solana::state::MappingAccount;
#[derive(Debug, Clone)]
pub struct ProductAccount {
    pub account_data:   pyth_sdk_solana::state::ProductAccount,
    pub price_accounts: Vec<Pubkey>,
}
pub type PriceAccount = pyth_sdk_solana::state::PriceAccount;

// Oracle is responsible for fetching Solana account data stored in the Pyth on-chain Oracle.
pub struct Oracle {
    config: Config,

    // The Solana account data
    data: Data,

    // The RPC client to use to poll data from the RPC node
    // Also pass in a websocket client to use for "get account data" if
    // websocket data is found.
    rpc_client: RpcClient,

    // The interval with which to poll for data
    poll_interval: Interval,

    // Channel on which account updates are received from the subscriber
    updates_rx: mpsc::Receiver<(Pubkey, solana_sdk::account::Account)>,

    // Channel on which updates are sent to the global store
    global_store_tx: mpsc::Sender<global::Update>,

    logger: Logger,
}

#[derive(Default, Deserialize)]
pub struct Config {
    /// The commitment level to use when reading data from the RPC node.
    pub commitment:               CommitmentLevel,
    /// Public key of the Oracle program.
    pub oracle_account_key:       Pubkey,
    /// Public key of the root mapping account.
    pub mapping_account_key:      Pubkey,
    /// RPC endpoint to send requests to.
    pub rpc_url:                  String,
    /// The interval with which to poll account information.
    pub poll_interval_duration:   Duration,
    /// Whether subscribing to account updates over websocket is enabled
    pub subscriber_enabled:       bool,
    /// Configuration for account Subscriber
    pub subscriber:               subscriber::Config,
    /// Capacity of the channel over which the Subscriber sends updates to the Exporter
    pub updates_channel_capacity: usize,
}

pub fn spawn_oracle(
    config: Config,
    global_store_update_tx: mpsc::Sender<global::Update>,
    logger: Logger,
) -> Vec<JoinHandle<()>> {
    // Create and spawn the account subscriber
    let (updates_tx, updates_rx) = mpsc::channel(config.updates_channel_capacity);
    let subscriber = Subscriber::new(config.subscriber.clone(), updates_tx, logger.clone());
    let subscriber_jh = tokio::spawn(async move { subscriber.run().await });

    // Create and spawn the Oracle
    let mut oracle = Oracle::new(config, updates_rx, global_store_update_tx, logger);
    let oracle_jh = tokio::spawn(async move { oracle.run().await });

    vec![subscriber_jh, oracle_jh]
}

impl Oracle {
    pub fn new(
        config: Config,
        updates_rx: mpsc::Receiver<(Pubkey, solana_sdk::account::Account)>,
        global_store_tx: mpsc::Sender<global::Update>,
        logger: Logger,
    ) -> Self {
        let rpc_client = RpcClient::new_with_commitment(
            config.rpc_url.clone(),
            CommitmentConfig {
                commitment: config.commitment,
            },
        );
        let poll_interval = tokio::time::interval(config.poll_interval_duration);

        Oracle {
            config,
            data: Default::default(),
            rpc_client,
            poll_interval,
            updates_rx,
            global_store_tx,
            logger,
        }
    }

    pub async fn run(&mut self) {
        loop {
            if let Err(err) = self.handle_next().await {
                error!(self.logger, "{:#}", err; "error" => format!("{:?}", err));
            }
        }
    }

    async fn handle_next(&mut self) -> Result<()> {
        tokio::select! {
            Some((account_key, account)) = self.updates_rx.recv() => {
                self.handle_account_update(&account_key, &account).await
            }
            _ = self.poll_interval.tick() => {
                self.poll().await
            }
        }
    }

    async fn poll(&mut self) -> Result<()> {
        self.data.mapping_accounts = self
            .fetch_mapping_accounts(self.config.mapping_account_key)
            .await?;
        self.data.product_accounts = self
            .fetch_product_accounts(self.data.mapping_accounts.values())
            .await?;
        self.data.price_accounts = self
            .fetch_price_accounts(self.data.product_accounts.values())
            .await?;

        self.send_all_data_to_global_store().await?;

        Ok(())
    }

    async fn fetch_mapping_accounts(
        &self,
        mapping_account_key: Pubkey,
    ) -> Result<HashMap<Pubkey, MappingAccount>> {
        let mut accounts = HashMap::new();

        let mut account_key = mapping_account_key;
        while account_key != Pubkey::default() {
            let account =
                *load_mapping_account(&self.rpc_client.get_account_data(&account_key).await?)?;
            accounts.insert(account_key, account);

            account_key = account.next;
        }

        Ok(accounts)
    }

    async fn fetch_product_accounts<'a, A>(
        &self,
        mapping_accounts: A,
    ) -> Result<HashMap<Pubkey, ProductAccount>>
    where
        A: IntoIterator<Item = &'a MappingAccount>,
    {
        let mut product_accounts = HashMap::new();

        for mapping_account in mapping_accounts {
            product_accounts.extend(
                self.fetch_product_accounts_from_mapping_account(mapping_account)
                    .await?,
            );
        }

        Ok(product_accounts)
    }

    async fn fetch_price_accounts<'a, P>(
        &self,
        product_accounts: P,
    ) -> Result<HashMap<Pubkey, PriceAccount>>
    where
        P: IntoIterator<Item = &'a ProductAccount>,
    {
        let mut price_accounts = HashMap::new();

        for product_account in product_accounts {
            for price_account_key in &product_account.price_accounts {
                let price_account = self.fetch_price_account(price_account_key).await?;
                price_accounts.insert(*price_account_key, price_account);
            }
        }

        Ok(price_accounts)
    }

    async fn fetch_product_accounts_from_mapping_account(
        &self,
        mapping_account: &MappingAccount,
    ) -> Result<HashMap<Pubkey, ProductAccount>> {
        let mut product_accounts = HashMap::new();

        for account_key in &mapping_account.products {
            // Update the price accounts
            let product_account = self.fetch_product_account(account_key).await?;
            product_accounts.insert(*account_key, product_account);
        }

        Ok(product_accounts)
    }

    async fn fetch_product_account(&self, product_account_key: &Pubkey) -> Result<ProductAccount> {
        // Fetch the product account
        let product_account = *load_product_account(
            &self
                .rpc_client
                .get_account_data(product_account_key)
                .await?,
        )?;

        // Fetch the price accounts associated with this product account
        let mut price_accounts = HashMap::new();
        let mut price_account_key = product_account.px_acc;
        while price_account_key != Pubkey::default() {
            let price_account = self.fetch_price_account(&price_account_key).await?;
            price_accounts.insert(price_account_key, price_account);

            price_account_key = price_account.next;
        }

        // Create the product account object
        let product_account = ProductAccount {
            account_data:   product_account,
            price_accounts: price_accounts.keys().cloned().collect(),
        };

        Ok(product_account)
    }

    async fn fetch_price_account(&self, price_account_key: &Pubkey) -> Result<PriceAccount> {
        let data = self.rpc_client.get_account_data(price_account_key).await?;
        let price_account = *load_price_account(&data)?;

        Ok(price_account)
    }

    async fn handle_account_update(
        &mut self,
        account_key: &Pubkey,
        account: &Account,
    ) -> Result<()> {
        // We are only interested in price account updates, all other types of updates
        // will be fetched using polling.
        if !self.data.price_accounts.contains_key(account_key) {
            return Ok(());
        }

        self.handle_price_account_update(account_key, account).await
    }

    async fn handle_price_account_update(
        &mut self,
        account_key: &Pubkey,
        account: &Account,
    ) -> Result<()> {
        let price_account = *load_price_account(&account.data)?;
        self.data.price_accounts.insert(*account_key, price_account);

        self.notify_price_account_update(account_key, &price_account)
            .await?;

        Ok(())
    }

    async fn send_all_data_to_global_store(&self) -> Result<()> {
        for (product_account_key, product_account) in &self.data.product_accounts {
            self.notify_product_account_update(product_account_key, product_account)
                .await?;
        }

        for (price_account_key, price_account) in &self.data.price_accounts {
            self.notify_price_account_update(price_account_key, price_account)
                .await?;
        }

        Ok(())
    }

    async fn notify_product_account_update(
        &self,
        account_key: &Pubkey,
        account: &ProductAccount,
    ) -> Result<()> {
        self.global_store_tx
            .send(global::Update::ProductAccountUpdate {
                account_key: account_key.clone(),
                account:     account.clone(),
            })
            .await
            .map_err(|_| anyhow!("failed to notify product account update"))
    }

    async fn notify_price_account_update(
        &self,
        account_key: &Pubkey,
        account: &PriceAccount,
    ) -> Result<()> {
        self.global_store_tx
            .send(global::Update::PriceAccountUpdate {
                account_key: account_key.clone(),
                account:     account.clone(),
            })
            .await
            .map_err(|_| anyhow!("failed to notify price account update"))
    }
}

mod subscriber {
    use {
        anyhow::{
            anyhow,
            Result,
        },
        serde::Deserialize,
        slog::Logger,
        solana_sdk::{
            account::Account,
            commitment_config::CommitmentLevel,
            pubkey::Pubkey,
        },
        solana_shadow::{
            BlockchainShadow,
            SyncOptions,
        },
        tokio::sync::{
            broadcast,
            mpsc,
        },
    };

    #[derive(Clone, Default, Deserialize)]
    pub struct Config {
        /// Commitment level used to read account data
        pub commitment:  CommitmentLevel,
        /// Public key of the root account to monitor. Note that all
        /// accounts owned by this account are also monitored.
        pub account_key: Pubkey,
        /// HTTP RPC endpoint
        pub rpc_url:     String,
        /// WSS RPC endpoint
        pub wss_url:     String,
    }

    /// Subscriber subscribes to all changes on the given account, and sends those changes
    /// on updates_tx. This is a convenience wrapper around the Blockchain Shadow crate.
    pub struct Subscriber {
        config: Config,

        // Channel on which updates are sent
        updates_tx: mpsc::Sender<(Pubkey, solana_sdk::account::Account)>,

        logger: Logger,
    }

    impl Subscriber {
        pub fn new(
            config: Config,
            updates_tx: mpsc::Sender<(Pubkey, solana_sdk::account::Account)>,
            logger: Logger,
        ) -> Self {
            Subscriber {
                config,
                updates_tx,
                logger,
            }
        }

        pub async fn run(&self) {
            match self.start_shadow().await {
                Ok(mut shadow_rx) => self.forward_updates(&mut shadow_rx).await,
                Err(err) => error!(self.logger, "{:#}", err; "error" => format!("{:?}", err)),
            }
        }

        async fn forward_updates(&self, shadow_rx: &mut broadcast::Receiver<(Pubkey, Account)>) {
            loop {
                if let Err(err) = self.forward_update(shadow_rx).await {
                    error!(self.logger, "{:#}", err; "error" => format!("{:?}", err))
                }
            }
        }

        async fn forward_update(
            &self,
            shadow_rx: &mut broadcast::Receiver<(Pubkey, Account)>,
        ) -> Result<()> {
            self.updates_tx
                .send(shadow_rx.recv().await?)
                .await
                .map_err(|_| anyhow!("failed to forward update"))
        }

        pub async fn start_shadow(
            &self,
        ) -> Result<broadcast::Receiver<(Pubkey, solana_sdk::account::Account)>> {
            let shadow = BlockchainShadow::new_for_program(
                &self.config.account_key,
                SyncOptions {
                    network: solana_shadow::Network::Custom(
                        self.config.rpc_url.clone(),
                        self.config.wss_url.clone(),
                    ),
                    commitment: self.config.commitment,
                    ..SyncOptions::default()
                },
            )
            .await?;

            Ok(shadow.updates_channel())
        }
    }
}