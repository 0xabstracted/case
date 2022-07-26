use std::str::FromStr;

use anchor_client::solana_sdk::pubkey::Pubkey;
use anyhow::Result;
use console::style;
use tars::{accounts as nft_accounts, instruction as nft_instruction};
use mpl_token_metadata::{pda::find_collection_authority_account, state::Metadata};

use crate::{
    cache::load_cache,
    tars::{TARS_ID, *},
    common::*,
    pdas::*,
    utils::{assert_correct_authority, spinner_with_style},
};

pub struct RemoveCollectionArgs {
    pub keypair: Option<String>,
    pub rpc_url: Option<String>,
    pub cache: String,
    pub tars: Option<String>,
}

pub fn process_remove_collection(args: RemoveCollectionArgs) -> Result<()> {
    let case_config = case_setup(args.keypair, args.rpc_url)?;
    let client = setup_client(&case_config)?;
    let program = client.program(TARS_ID);
    let mut cache = Cache::new();

    // the tars id specified takes precedence over the one from the cache
    let tars_id = match args.tars {
        Some(ref tars_id) => tars_id,
        None => {
            cache = load_cache(&args.cache, false)?;
            &cache.program.tars
        }
    };

    let tars_pubkey = match Pubkey::from_str(tars_id) {
        Ok(tars_pubkey) => tars_pubkey,
        Err(_) => {
            let error = anyhow!("Failed to parse tars id: {}", tars_id);
            error!("{:?}", error);
            return Err(error);
        }
    };

    println!(
        "{} {}Loading tars",
        style("[1/2]").bold().dim(),
        LOOKING_GLASS_EMOJI
    );
    println!("{} {}", style("Tars ID:").bold(), tars_id);

    let pb = spinner_with_style();
    pb.set_message("Connecting...");

    let tars_state = get_tars_state(&case_config, &tars_pubkey)?;
    let (collection_pda_pubkey, collection_pda) = get_collection_pda(&tars_pubkey, &program)?;
    let collection_mint_pubkey = collection_pda.mint;
    let collection_metadata_info = get_metadata_pda(&collection_mint_pubkey, &program)?;

    pb.finish_with_message("Done");

    assert_correct_authority(
        &case_config.keypair.pubkey(),
        &tars_state.authority,
    )?;

    println!(
        "\n{} {}Removing collection mint for tars",
        style("[2/2]").bold().dim(),
        TARS_EMOJI
    );

    let pb = spinner_with_style();
    pb.set_message("Sending remove collection transaction...");

    let remove_signature = remove_collection(
        &program,
        &tars_pubkey,
        &tars_state,
        &collection_pda_pubkey,
        &collection_mint_pubkey,
        &collection_metadata_info,
    )?;

    // If a tars id wasn't manually specified we are operating on the tars in the cache
    // and so need to update the cache file.
    if args.tars.is_none() {
        cache.items.shift_remove("-1");
        cache.program.collection_mint = String::new();
        cache.sync_file()?;
    }

    pb.finish_with_message(format!(
        "{} {}",
        style("Remove collection signature:").bold(),
        remove_signature
    ));

    Ok(())
}

pub fn remove_collection(
    program: &Program,
    tars_pubkey: &Pubkey,
    tars_state: &Tars,
    collection_pda_pubkey: &Pubkey,
    collection_mint_pubkey: &Pubkey,
    collection_metadata_info: &PdaInfo<Metadata>,
) -> Result<Signature> {
    let payer = program.payer();

    let collection_authority_record =
        find_collection_authority_account(collection_mint_pubkey, collection_pda_pubkey).0;

    let (collection_metadata_pubkey, collection_metadata) = collection_metadata_info;

    if collection_metadata.update_authority != payer {
        return Err(anyhow!(CustomTarsError::AuthorityMismatch(
            collection_metadata.update_authority.to_string(),
            payer.to_string()
        )));
    }

    if tars_state.items_redeemed > 0 {
        return Err(anyhow!(
            "You can't modify the Tars collection after items have been minted."
        ));
    }

    let builder = program
        .request()
        .accounts(nft_accounts::RemoveCollection {
            tars: *tars_pubkey,
            authority: payer,
            collection_pda: *collection_pda_pubkey,
            metadata: *collection_metadata_pubkey,
            mint: *collection_mint_pubkey,
            collection_authority_record,
            token_metadata_program: mpl_token_metadata::ID,
        })
        .args(nft_instruction::RemoveCollection);

    let sig = builder.send()?;

    Ok(sig)
}
