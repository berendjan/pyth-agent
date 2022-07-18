pragma circom 2.0.0;

// Tricks&Notes on Circom:
// - Scalar size errors happen when not providing all signals.
// - Power of Tau ceremony must choose an exponent e such that 2**e > constraint count.
// - C++ errors will fail constraints even before proof is produced. Asserts.
// - At least one quadratic output seems to be needed, so a squared input is used here.
// - TODO: Security; understand constraining outputs and when needed. See operators page on Circom.
//
// Goals
// - Faster Proving Times
// - Dig more into Curve support and what our limitations are (currently BN254).
// - Proof of concept P2P Protocol (even without P2P is fine).

// - [x] Timestamp oracle as a median.
// - [x] Fee for proving.
// - [x] Staleness threshold on price inputs.
// - [x] Publishers commit to timestamps.
// - [x] Publishers commit to observed online amount.
// - [ ] Check the signatures are from different publishers 
// - [ ] refactor code to template / functions
// - [ ] checks for subgroup order 
// - [ ] Min pub, required.
// - [ ] Contract with N prices must work for <N. Dynamic N.

include "node_modules/circomlib/circuits/comparators.circom";
include "node_modules/circomlib/circuits/gates.circom";

include "lib/SortedArray.circom";
include "lib/Median.circom";
include "InputVerifier.circom";
include "PriceModel.circom"; 

//check conditions to ensure that elements up to the expected length 
//are nonempty and everything above is empty 
function checkLength(arr, MAX, N) {
    var EMPTY = -1; 
    component xor_cond[MAX]; 
    component and_cond[MAX]; 
    component and_cond2[MAX];   

    for (var i = 0; i < MAX: i++) {
        var p = i >= N; 
        var q = arr[i] == EMPTY;   

        var p1 = i < N; 
        var q1 = arr[i] != EMPTY;   

        xor_cond[i] = XOR(); 
        and_cond[i] = AND(); 
        and_cond2[i] = AND();

        and_cond[i].a <-- p; 
        and_cond[i].b <-- q; 
        and_cond.out <== 1; 
        
        and_cond2[i].a <-- p1; 
        and_cond2[i].b <-- q1; 
        and_cond2.out <== 1; 

        xor_cond[i].a <-- and_cond[i]; 
        xor_cond[i].b <-- and_cond2[i] 
        xor_cond.out <== 1; 


    }

}

function calc_price(price_model, prices, confs, i) {
    var price = prices[price_model[i][0]];
    var conf  = confs[price_model[i][1]];
    var op    = price_model[i][2];

    if(op == 0) {
        return price - conf;
    } else if (op == 1) {
        return price;
    } else {
        return price + conf;
    }
}

// Proof is per-price
template Pyth(Max, timestampThreshold, validPubKeys) {

    /*
        Template Inputs 
        Max - maximum number of components included in the proof
        timestampThreshold - staleness threshold for data feed aggregation
        validPubKeys - array of valid public keys
    */

    // Publisher Controlled Inputs:
    //
    // Requirements:
    // Check all array elements are sorted.
    // Check all array elements are non-zero up to N. (0 indicates NULL).
    // Check all array elements are zero (NULL) >= N.
    //
    // These requirements allow us to prove aggregations for a variable number
    // of elements. 
    //max = 10 
    // N = 8
    //thresh = 7 
    signal input    N;

    signal input  price_model[Max*3][3];
    signal input  prices[Max];
    signal input  confs[Max];
    signal input  timestamps[Max];
    signal input  observed_online[Max];

    // Signatures: A/R/S components are part of the ed25519 signature scheme.
    // 
    // NOTE: The hash used in ed25519 in this contract uses the MiMC hash
    //       function rather than SHA256 as in standard ed25519. You can use
    //       circomlibjs to produce signatures that match this algorithm.
    signal input  A[Max][256];
    signal input  R[Max][256];
    signal input  S[Max][256];

    // Output p-values for aggregattion.
    signal output p25;
    signal output p50;
    signal output p75;

    // Width of the confidence interval around the p50 aggregate.
    signal output confidence; 
    // Return fee input as output for verification contracts to charge users.
    signal input  fee;
    //threshold - number of signatures we need to include 
    //FILTER the input data
    //checks length / vals of the signal inputs 
    checkLength(prices, MAX, N); 
    checkLength(confs, MAX, N); 
    checkLength(timestamps, MAX, N); 
    checkLength(observed_online, MAX, N);    
    //set the last bit to -1 in the S component of the ED25519 signature 
    //last 3 bits need to be 0 for a signature to be non-malleable 
    //(curve order size ~< last 3 bits) 

    //check last bit 
    LastBitsSignatures[MAX]; 
    for (var i = 0; i < MAX; i++) {      
        LastBitsSignatures[i] <-- S[i][255];     
    }
    checkLength(LastBitsSignatures, MAX, N);
    
    // In order to prevent the prover from choosing 
    component timestamp_median = Median(Max);
    for(var i = 0; i < Max; i++) {
        timestamp_median.list[i] <== timestamps[i];
    }


    // All timestamps must be within a certain range of the median.
    // TODO: Does it have to be bits?
    component timestamp_gt[Max];
    component timestamp_lte[Max];
    for(var i = 0; i < Max; i++) timestamp_lte[i] = LessEqThan(64);
    for(var i = 0; i < Max; i++) timestamp_gt[i]  = GreaterThan(64);

    for(var i = 0; i < Max; i++) {
        timestamp_lte[i].in[0] <== timestamps[i];
        timestamp_lte[i].in[1] <== timestamp_median.result;
        timestamp_gt[i].in[0]  <== timestamps[i];
        timestamp_gt[i].in[1]  <== timestamp_median.result - TimestampThreshold;
    }


    // Convert Price/Confidence pairs into binary encoded values for signature
    // verification.
    component Num2Bits_price_components[Max];
    component Num2Bits_conf_components[Max];
    component Num2Bits_timestamp_components[Max];
    component Num2Bits_online_components[Max];
    for(var i=0; i<Max; i++) {
        Num2Bits_price_components[i] = Num2Bits(64);
        Num2Bits_conf_components[i]  = Num2Bits(64);
        Num2Bits_timestamp_components[i] = Num2Bits(64);
        Num2Bits_online_components[i] = Num2Bits(64);

        Num2Bits_price_components[i].in <== prices[i];
        Num2Bits_conf_components[i].in  <== confs[i];
        Num2Bits_timestamp_components[i].in <== timestamps[i];
        Num2Bits_online_components[i].in <== observed_online[i];
    }

    // Verify the encoded data against incoming signatures.
    component verifiers[Max];
    for(var i = 0; i < Max; i++) {
        verifiers[i] = InputVerifier();

        // Assign output of binary conversion to signature verifier.
        for (var j = 0; j < 64; j++) {
            verifiers[i].price[j]      <== Num2Bits_price_components[i].out[j];
            verifiers[i].confidence[j] <== Num2Bits_conf_components[i].out[j];
            verifiers[i].timestamp[j]  <== Num2Bits_timestamp_components[i].out[j];
            verifiers[i].online[j]     <== Num2Bits_online_components[i].out[j];
        }

        // Assign Signature Components.
        for (var j = 0; j < 256; j++) {
            verifiers[i].A[j] <== A[i][j];
            verifiers[i].R[j] <== R[i][j];
            verifiers[i].S[j] <== S[i][j];
        }
    }

    // We verify that the price_model has been given to us in order by iterating
    // over the signal set and checking that every element is smaller than its
    // successor. I.E: all(map(lambda a, b: a <= b, prices))
    signal sort_checks[Max*3];
    for(var i=1; i<Max*3; i++) {
        var a = calc_price(price_model, prices, confs, i-1);
        var b = calc_price(price_model, prices, confs, i);

        // Constrain r1 < r2
        sort_checks[i] <-- a <= b;
        sort_checks[i] === 1;
    }

    component price_calc = PriceModelCore(Max*3);
    for(var i=0; i<Max*3; i++) {
        // TODO: Constraint missing, do we need one? <-- dangerous.
        price_calc.prices[i] <-- calc_price(price_model, prices, confs, i);
    }

    // Calculate confidence from aggregation result.
    var agg_conf_left  = price_calc.agg_p50 - price_calc.agg_p25;
    var agg_conf_right = price_calc.agg_p75 - price_calc.agg_p50;
    var agg_conf       =
        agg_conf_right > agg_conf_left
            ? agg_conf_right
            : agg_conf_left;

    signal confidence_1 <-- agg_conf_right > agg_conf_left;
    signal confidence_2 <== confidence_1*agg_conf_right;
    signal confidence_3 <== (1-confidence_1)*agg_conf_left;

    confidence <== confidence_2 + confidence_3;
    p25        <== price_calc.agg_p25;
    p50        <== price_calc.agg_p50;
    p75        <== price_calc.agg_p75;
 }

component main = Pyth(10, 10);
