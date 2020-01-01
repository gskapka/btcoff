use bitcoin::{
    hashes::{
        Hash,
        sha256d,
        hash160,
    },
    util::{
        key::PublicKey as BtcPublicKey,
        address::Address as BtcAddress,
    },
    network::constants::Network as BtcNetwork,
    consensus::encode::serialize as btc_serialize,
    consensus::encode::deserialize as btc_deserialize,
    blockdata::{
        opcodes,
        block::Block as BtcBlock,
        transaction::{
            TxIn as BtcUtxo,
            TxOut as BtcTxOut,
            Transaction as BtcTransaction,
        },
        script::{
            Script as BtcScript,
            Builder as BtcScriptBuilder,
        },
    },
};
use crate::{
    state::State,
    errors::AppError,
    get_cli_args::{
        CliArgs,
        get_nonce_from_cli_arg,
    },
    btc_private_key::BtcPrivateKey,
    get_btc_private_key::get_btc_private_key_and_add_to_state,
    utxo_codec::get_utxos_from_utxo_json_string_and_add_to_state,
    get_utxo_json_string::get_utxo_json_string_from_cli_args_and_add_to_state,
    types::{
        Bytes,
        Result,
        BtcUtxoAndValue,
    },
    utils::{
        calculate_btc_tx_fee,
        create_new_tx_output,
        get_total_value_of_utxos_and_values,
        get_change_address_from_cli_args_in_state,
    },
    get_pbtc_deposit_address::{
        generate_pbtc_script_sig,
        get_eth_address_and_nonce_hash,
        get_eth_address_from_cli_args_and_put_in_state,
    },
};

pub fn get_pbtc_script_sig<'a>(
    signature_slice: &'a[u8],
    redeem_script: &BtcScript,
) -> BtcScript {
    let script_builder = BtcScriptBuilder::new();
    script_builder
        .push_slice(&signature_slice)
        .push_slice(redeem_script.as_bytes())
        .into_script()
}

pub fn get_redeem_script_for_signing<'a>(
    redeem_script: &BtcScript,
) -> BtcScript {
    let script_builder = BtcScriptBuilder::new();
    script_builder
        .push_slice(redeem_script.as_bytes())
        .into_script()
}

fn get_btc_script_and_put_in_state(state: State) -> Result<State> {
    info!("✔ Getting BTC redeem script and putting in state...");
    generate_pbtc_script_sig( // TODO Rename to "redeem script" or something? 
        &state.get_btc_private_key()?.to_p2pkh_btc_address(),
        &state.get_btc_private_key()?.to_public_key_slice(),
        state.get_eth_address_and_nonce_hash()?
    ) 
        .and_then(|script| state.add_btc_script(script))
}

pub fn get_eth_address_and_nonce_hash_and_put_in_state(
    state: State
) -> Result<State> {
    get_eth_address_and_nonce_hash(
        state.get_eth_address_bytes()?, 
        // FIXME: Only using the first things we've passed in!
        &get_nonce_from_cli_arg(&state.cli_args.arg_ethAddressNonce[0])?,
    )
        .and_then(|hash| state.add_eth_address_and_nonce_hash(hash))
}

fn make_pbtc_tx_and_put_in_state(
    state: State
) -> Result<State> {
    info!("✔ Making pBTC tx and putting in state...");
    create_signed_raw_btc_tx_for_n_input_n_outputs(
        state.cli_args.flag_fee.clone(),
        state.addresses_and_amounts.clone(),
        &get_change_address_from_cli_args_in_state(&state)?,
        state.get_btc_private_key()?.clone(),
        state.get_btc_utxos_and_values()?.clone(),
        None,
        state.get_btc_script()?,
    )
        .and_then(|tx| state.add_btc_tx(tx))
}

pub fn make_pbtc_utxo_tx(cli_args: CliArgs) -> Result<String> {
    info!("✔ Spending pBTC UTXO(s)...");

    // TODO FIXME RM!
    let hash = hash160::Hash::hash(&hex::decode("232103d2a5e3b162eb580fe2ce023cd5e0dddbb6286923acde77e3e5468314dc9373f7ac").unwrap());
    debug!("hash 160 of serialized redeem: {}", hash);

    State::init_from_cli_args(cli_args.clone())
        .and_then(get_btc_private_key_and_add_to_state)
        .and_then(get_utxo_json_string_from_cli_args_and_add_to_state)
        .and_then(get_utxos_from_utxo_json_string_and_add_to_state)
        .and_then(get_eth_address_from_cli_args_and_put_in_state)
        .and_then(get_eth_address_and_nonce_hash_and_put_in_state)
        .and_then(get_btc_script_and_put_in_state)
        .and_then(make_pbtc_tx_and_put_in_state)
        .and_then(|state| Ok(hex::encode(btc_serialize(state.get_btc_tx()?))))
}

pub const VERSION: u32 = 1;
pub const LOCK_TIME: u32 = 0;
pub const SIGN_ALL_HASH_TYPE: u8 = 1;

pub static UTXO_VALUE_TOO_LOW_ERROR: &'static str =
    "✘ Not enough UTXO value to make transaction!";
/*
 * So something else to try: this stack overflow thingy:
 * https://bitcoin.stackexchange.com/questions/66197/step-by-step-example-to-redeem-a-p2sh-output-required
 * mentions that they set the redeem script as the sig_scripts before signing.
 * BUt apparently I'm doing the right thing with the final setting of 
 * sig_script to [sig] [serialized redeem script]
 */
pub fn create_signed_raw_btc_tx_for_n_input_n_outputs(
    sats_per_byte: usize,
    recipient_addresses_and_amounts: Vec<(String, u64)>, // TODO MAKE A TYPE?
    remainder_btc_address: &str,
    btc_private_key: BtcPrivateKey,
    utxos_and_values: Vec<BtcUtxoAndValue>,
    maybe_op_return_output: Option<BtcTxOut>,
    redeem_script: &BtcScript,
) -> Result<BtcTransaction> {
    debug!("Redeem script: {}", redeem_script);
    debug!("Redeem script serialized: {}", hex::encode(redeem_script.as_bytes()));
    let total_to_spend: u64 = recipient_addresses_and_amounts
        .iter()
        .map(|(_, amount)| amount)
        .sum();

    let fee = calculate_btc_tx_fee(
        utxos_and_values.len(),
        match &maybe_op_return_output {
            None => recipient_addresses_and_amounts.len(),
            Some(_) => recipient_addresses_and_amounts.len() + 1,
        },
        sats_per_byte
    );
    let utxo_total = get_total_value_of_utxos_and_values(&utxos_and_values);
    info!("✔ UTXO(s) total:  {}", utxo_total);
    info!("✔ Outgoing total: {}", total_to_spend);
    info!("✔ Change amount:  {}", utxo_total - (total_to_spend + fee));
    info!("✔ Tx fee:         {}", fee);
    match total_to_spend + fee > utxo_total {
        true => return Err(AppError::Custom(
            UTXO_VALUE_TOO_LOW_ERROR.to_string()
        )),
        _ => {
            let mut outputs = recipient_addresses_and_amounts
                .iter()
                .map(|(address, amount)| create_new_tx_output(&amount, address))
                .collect::<Result<Vec<BtcTxOut>>>()?;
            if let Some(op_return_output) = maybe_op_return_output {
                outputs.push(op_return_output);
            };
            let change = utxo_total - total_to_spend - fee;
            if change > 0 {
                outputs.push(
                    create_new_tx_output(&change, remainder_btc_address)?
                )
            };
            let tx = BtcTransaction {
                output: outputs,
                version: VERSION,
                lock_time: LOCK_TIME,
                input: utxos_and_values
                    .iter()
                    .map(|utxo_and_value| utxo_and_value.get_utxo())
                    .collect::<Result<Vec<BtcUtxo>>>()?,
            };
            let signatures = utxos_and_values
                .iter()
                .map(|utxo_and_value| utxo_and_value.get_utxo())
                .collect::<Result<Vec<BtcUtxo>>>()?
                .iter()
                .enumerate()
                .map(|(i, utxo)|
                    tx.signature_hash(
                        i,
                        //&utxo.script_sig,
                        // so this should be the redeem script?
                        // Changing these only changes the signature, obvs
                        &redeem_script,
                        //&get_redeem_script_for_signing(redeem_script),
                        /*
                         * one more thing to try:
                         * set this as a script that just pushed the serialized
                         * redeem script to it, not the actual script itself!
                         * If that doesn't work, back to the drawing board.
                         */
                        SIGN_ALL_HASH_TYPE as u32
                    )
                )
                .map(|hash| hash.to_vec())
                .map(|tx_hash_to_sign|
                    btc_private_key
                        .sign_hash_and_append_btc_hash_type(
                            tx_hash_to_sign.to_vec(),
                            SIGN_ALL_HASH_TYPE as u8,
                        )
                )
                .collect::<Result<Vec<Bytes>>>()?;
            let utxos_with_signatures = utxos_and_values
                .iter()
                .map(|utxo_and_value| utxo_and_value.get_utxo())
                .collect::<Result<Vec<BtcUtxo>>>()?
                .iter()
                .enumerate()
                .map(|(i, utxo)|
                    BtcUtxo {
                        sequence: utxo.sequence,
                        witness: utxo.witness.clone(),
                        previous_output: utxo.previous_output,
                        // NOTE: The following is what differs from normal tx!
                        script_sig: get_pbtc_script_sig(
                            &signatures[i],
                            &redeem_script,
                        ),
                    }
                 )
                .collect::<Vec<BtcUtxo>>();
            Ok(
                BtcTransaction {
                    output: tx.output,
                    version: tx.version,
                    lock_time: tx.lock_time,
                    input: utxos_with_signatures,
                }
            )
        }
    }
}
