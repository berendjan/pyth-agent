use {
    super::{
        solana::oracle::PriceEntry,
        store::{
            global::{
                AllAccountsData,
                AllAccountsMetadata,
                Lookup,
                PriceAccountMetadata,
            },
            local::{
                Message,
                PriceInfo,
            },
        },
    },
    crate::agent::metrics::MetricsServer,
    chrono::NaiveDateTime,
    pyth_sdk::{
        Identifier,
        PriceIdentifier,
    },
    slog::Logger,
    solana_sdk::pubkey::Pubkey,
    std::{
        collections::{
            BTreeMap,
            BTreeSet,
            HashMap,
            HashSet,
        },
        time::Duration,
    },
    tokio::sync::oneshot,
    typed_html::{
        dom::DOMTree,
        html,
        text,
    },
};

impl MetricsServer {
    /// Create an HTML view of store data
    pub async fn render_dashboard(&self) -> Result<String, Box<dyn std::error::Error>> {
        // Prepare response channel for requests
        let (local_tx, local_rx) = oneshot::channel();
        let (global_data_tx, global_data_rx) = oneshot::channel();
        let (global_metadata_tx, global_metadata_rx) = oneshot::channel();

        // Request price data from local and global store
        self.local_store_tx
            .send(Message::LookupAllPriceInfo {
                result_tx: local_tx,
            })
            .await?;

        self.global_store_lookup_tx
            .send(Lookup::LookupAllAccountsData {
                result_tx: global_data_tx,
            })
            .await?;

        self.global_store_lookup_tx
            .send(Lookup::LookupAllAccountsMetadata {
                result_tx: global_metadata_tx,
            })
            .await?;

        // Await the results
        let local_data = local_rx.await?;
        let global_data = global_data_rx.await??;
        let global_metadata = global_metadata_rx.await??;

        let symbol_view =
            build_dashboard_data(local_data, global_data, global_metadata, &self.logger);

        // Note the uptime and adjust to whole seconds for cleaner output
        let uptime = Duration::from_secs(self.start_time.elapsed().as_secs());

        // Build and collect table rows
        let mut rows = vec![];

        for (symbol, data) in symbol_view {
            for (price_pubkey, price_data) in data.prices {
                let price_string = if let Some(global_data) = price_data.global_data {
                    let expo = global_data.expo;
                    let price_with_expo: f64 = global_data.agg.price as f64 * 10f64.powi(expo);
                    format!("{:.2}", price_with_expo)
                } else {
                    "no data".to_string()
                };

                let last_publish_string = if let Some(global_data) = price_data.global_data {
                    if let Some(datetime) =
                        NaiveDateTime::from_timestamp_opt(global_data.timestamp, 0)
                    {
                        datetime.format("%Y-%m-%d %H:%M:%S").to_string()
                    } else {
                        format!("Invalid timestamp {}", global_data.timestamp)
                    }
                } else {
                    "no data".to_string()
                };

                let last_local_update_string = if let Some(local_data) = price_data.local_data {
                    if let Some(datetime) =
                        NaiveDateTime::from_timestamp_opt(local_data.timestamp, 0)
                    {
                        datetime.format("%Y-%m-%d %H:%M:%S").to_string()
                    } else {
                        format!("Invalid timestamp {}", local_data.timestamp)
                    }
                } else {
                    "no data".to_string()
                };

                let row_snippet = html! {
                            <tr>
                                <td>{text!(symbol.clone())}</td>
                                <td>{text!(data.product.to_string())}</td>
                <td>{text!(price_pubkey.to_string())}</td>
                <td>{text!(price_string)}</td>
                <td>{text!(last_publish_string)}</td>
                <td>{text!(last_local_update_string)}</td>
                            </tr>
                            };
                rows.push(row_snippet);
            }
        }

        let title_string = concat!("Pyth Agent Dashboard - ", env!("CARGO_PKG_VERSION"));
        let res_html: DOMTree<String> = html! {
        <html>
            <head>
            <title>{text!(title_string)}</title>
        <style>
            """
table {
  width: 100%;
  border-collapse: collapse;
}
table, th, td {
  border: 1px solid;
}
"""
        </style>
            </head>
            <body>
            <h1>{text!(title_string)}</h1>
        {text!("Uptime: {}", humantime::format_duration(uptime))}
            <h2>"State Overview"</h2>
            <table>
            <tr>
                <th>"Symbol"</th>
                <th>"Product ID"</th>
                <th>"Price ID"</th>
                <th>"Last Published Price"</th>
        <th>"Last Publish Time"</th>
        <th>"Last Local Update Time"</th>
            </tr>
            { rows }
        </table>
            </body>
        </html>
        };
        Ok(res_html.to_string())
    }
}

#[derive(Debug)]
pub struct DashboardSymbolView {
    product: Pubkey,
    prices:  BTreeMap<Pubkey, DashboardPriceView>,
}

#[derive(Debug)]
pub struct DashboardPriceView {
    local_data:      Option<PriceInfo>,
    global_data:     Option<PriceEntry>,
    global_metadata: Option<PriceAccountMetadata>,
}

/// Turn global/local store state into a single per-symbol view.
///
/// The dashboard data comes from three sources - the global store
/// (observed on-chain state) data, global store metadata and local
/// store data (local state possibly not yet committed to the oracle
/// contract).
///
/// The view is indexed by human-readable symbol name or a stringified
/// public key if symbol name can't be found.
pub fn build_dashboard_data(
    mut local_data: HashMap<PriceIdentifier, PriceInfo>,
    mut global_data: AllAccountsData,
    mut global_metadata: AllAccountsMetadata,
    logger: &Logger,
) -> BTreeMap<String, DashboardSymbolView> {
    let mut ret = BTreeMap::new();

    debug!(logger, "Building dashboard data";
      "local_data_len" => local_data.len(),
      "global_data_products_len" => global_data.product_accounts.len(),
      "global_data_prices_len" => global_data.price_accounts.len(),
      "global_metadata_products_len" => global_metadata.product_accounts_metadata.len(),
      "global_metadata_prices_len" => global_metadata.price_accounts_metadata.len(),
    );

    // Learn all the product/price keys in the system,
    let all_product_keys_iter = global_metadata.product_accounts_metadata.keys().cloned();

    let all_product_keys_dedup = all_product_keys_iter.collect::<HashSet<Pubkey>>();

    let all_price_keys_iter = global_data
        .price_accounts
        .keys()
        .chain(global_metadata.price_accounts_metadata.keys())
        .cloned()
        .chain(local_data.keys().map(|identifier| {
            let bytes = identifier.to_bytes();
            Pubkey::new_from_array(bytes)
        }));

    let mut all_price_keys_dedup = all_price_keys_iter.collect::<HashSet<Pubkey>>();

    // query all the keys and assemvle them into the view

    let mut remaining_product_keys = all_product_keys_dedup.clone();

    for product_key in all_product_keys_dedup {
        let _product_data = global_data.product_accounts.remove(&product_key);

        if let Some(mut product_metadata) = global_metadata
            .product_accounts_metadata
            .remove(&product_key)
        {
            let mut symbol_name = product_metadata
                .attr_dict
                .get("symbol")
                .cloned()
                // Use product key for unnamed products
                .unwrap_or(format!("unnamed product {}", product_key));

            // Sort and deduplicate prices
            let this_product_price_keys_dedup = product_metadata
                .price_accounts
                .drain(0..)
                .collect::<BTreeSet<_>>();

            let mut prices = BTreeMap::new();

            // Extract information about each price
            for price_key in this_product_price_keys_dedup {
                let price_global_data = global_data.price_accounts.remove(&price_key);
                let price_global_metadata =
                    global_metadata.price_accounts_metadata.remove(&price_key);

                let price_identifier = Identifier::new(price_key.clone().to_bytes());
                let price_local_data = local_data.remove(&price_identifier);

                prices.insert(
                    price_key,
                    DashboardPriceView {
                        local_data:      price_local_data,
                        global_data:     price_global_data,
                        global_metadata: price_global_metadata,
                    },
                );
                // Mark this price as done
                all_price_keys_dedup.remove(&price_key);
            }

            // Mark this product as done
            remaining_product_keys.remove(&product_key);

            let symbol_view = DashboardSymbolView {
                product: product_key,
                prices,
            };

            if ret.contains_key(&symbol_name) {
                let new_symbol_name = format!("{} (duplicate)", symbol_name);

                warn!(logger, "Dashboard: duplicate symbol name detected, renaming";
                "symbol_name" => &symbol_name,
                "symbol_renamed_to" => &new_symbol_name,
                "conflicting_symbol_data" => format!("{:?}", symbol_view),
                );

                symbol_name = new_symbol_name;
            }

            ret.insert(symbol_name, symbol_view);
        } else {
            // This logging handles only missing products that we
            // should have found. Missing prices are okay, appearing
            // in cases where no on-chain queries or publishing took
            // place yet.
            warn!(logger, "Dashboard: Failed to look up product metadata"; "product_id" => product_key.to_string());
        }
    }

    if !(all_price_keys_dedup.is_empty() && remaining_product_keys.is_empty()) {
        let remaining_products: Vec<_> = remaining_product_keys.drain().collect();
        let remaining_prices: Vec<_> = all_price_keys_dedup.drain().collect();
        warn!(logger, "Dashboard: Orphaned product/price IDs detected";
	      "remaining_product_ids" => format!("{:?}", remaining_products),
	      "remaining_price_ids" => format!("{:?}", remaining_prices));
    }

    return ret;
}
