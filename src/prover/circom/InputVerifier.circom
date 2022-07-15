pragma circom 2.0.0;

include "node_modules/circomlib/circuits/eddsa.circom";

// InputVerifier receives a list of signatures, prices, and components, and
// verifies that the prices and components have been correctly signed by the
// corresponding signature public keys.
//
// The price and component at position `i`, must correspond to the signature
// at position `i`.
//
// The `N` parameter refers to the number of price components the circuit is
// able to process, and is fixed at compile time.
template InputVerifier() {
    // Components of the input ed25519 signatures. Used by ed25519.circom
    signal input A[256];
    signal input R[256];
    signal input S[256];

    // Price and confidence components are encoded as 64 bit binary integers as
    // ed25519 requires binary inputs.
    signal input price[64];
    signal input confidence[64];
    signal input timestamp[64];
    signal input online[64];

    // Publishers sign price and confidence with the following code:
    //
    //   `signature = ed25519::sign(price || confidence, secret_key)`
    //
    // Therefore we must also create a verifier that can verify the signature
    // of these 128 bit messages.
    component verifier;
    verifier = EdDSAVerifier(128);

    // Each component verifies one signature. If verification fails the
    // component will violate a constraint.

    // Create and assign signature messages to the EdDSA verifier.
    for(var i = 0; i < 64; i++) verifier.msg[i]     <== price[i];
    for(var i = 0; i < 64; i++) verifier.msg[64+i] <== confidence[i];

    // Assign the expected signature to the verifier.
    for(var j = 0; j < 256; j++) {
        verifier.A[j]  <== A[j];
        verifier.R8[j] <== R[j];
        verifier.S[j]  <== S[j];
    }
}
