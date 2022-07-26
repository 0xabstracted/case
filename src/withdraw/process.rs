use std::{rc::Rc, str::FromStr};

pub use anchor_client::{
    solana_sdk::{
        commitment_config::{CommitmentConfig, CommitmentLevel},
        native_token::LAMPORTS_PER_SOL,
        pubkey::Pubkey,
        signature::{Keypair, Signature, Signer},
        system_instruction, system_program, sysvar,
        transaction::Transaction,
    },
    Client, Program,
};
use console::{style, Style};
use dialoguer::{theme::ColorfulTheme, Confirm};
use tars::{accounts as nft_accounts, instruction as nft_instruction};
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
};

use crate::{
    tars::TARS_ID,
    common::*,
    setup::{setup_client, case_setup},
    utils::*,
};

pub struct WithdrawArgs {
    pub tars: Option<String>,
    pub keypair: Option<String>,
    pub rpc_url: Option<String>,
    pub list: bool,
}

pub fn process_withdraw(args: WithdrawArgs) -> Result<()> {
    // (1) Setting up connection

    println!(
        "{} {}Initializing connection",
        style("[1/2]").bold().dim(),
        COMPUTER_EMOJI
    );

    let pb = spinner_with_style();
    pb.set_message("Connecting...");

    let (program, payer) = setup_withdraw(args.keypair, args.rpc_url)?;

    pb.finish_with_message("Connected");

    println!(
        "\n{} {}{} funds",
        style("[2/2]").bold().dim(),
        WITHDRAW_EMOJI,
        if args.list { "Listing" } else { "Retrieving" }
    );

    // the --list flag takes precedence; even if a tars id is passed
    // as an argument, we will list the tarss (no draining happens)
    let tars = if args.list { None } else { args.tars };

    // (2) Retrieving data for listing/draining

    match &tars {
        Some(tars) => {
            let tars = Pubkey::from_str(tars)?;

            let pb = spinner_with_style();
            pb.set_message("Draining tars...");

            do_withdraw(Rc::new(program), tars, payer)?;

            pb.finish_with_message("Done");
        }
        None => {
            let config = RpcProgramAccountsConfig {
                filters: Some(vec![RpcFilterType::Memcmp(Memcmp {
                    offset: 8, // key
                    bytes: MemcmpEncodedBytes::Base58(payer.to_string()),
                    encoding: None,
                })]),
                account_config: RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64),
                    data_slice: None,
                    commitment: Some(CommitmentConfig {
                        commitment: CommitmentLevel::Confirmed,
                    }),
                },
                with_context: None,
            };

            let pb = spinner_with_style();
            pb.set_message("Looking up tarss...");

            let program = Rc::new(program);
            let accounts = program
                .rpc()
                .get_program_accounts_with_config(&program.id(), config)?;

            pb.finish_and_clear();

            let mut total = 0.0f64;

            accounts.iter().for_each(|account| {
                let (_pubkey, account) = account;
                total += account.lamports as f64;
            });

            println!(
                "\nFound {} tarss, total amount: ◎ {}",
                accounts.len(),
                total / LAMPORTS_PER_SOL as f64
            );

            if !accounts.is_empty() {
                if args.list {
                    println!("\n{:48} Balance", "Tars ID");
                    println!("{:-<61}", "-");

                    for (pubkey, account) in accounts {
                        println!(
                            "{:48} {:>12.8}",
                            pubkey.to_string(),
                            account.lamports as f64 / LAMPORTS_PER_SOL as f64
                        );
                    }
                } else {
                    let warning = format!(
                        "\n\
                        +-----------------------------------------------------+\n\
                        | {} WARNING: This will drain ALL your Tarss |\n\
                        +-----------------------------------------------------+",
                        WARNING_EMOJI
                    );

                    println!("{}\n", style(warning).bold().yellow());

                    let theme = ColorfulTheme {
                        success_prefix: style("✔".to_string()).yellow().force_styling(true),
                        values_style: Style::new().yellow(),
                        ..get_dialoguer_theme()
                    };

                    if !Confirm::with_theme(&theme)
                        .with_prompt("Do you want to continue?")
                        .interact()?
                    {
                        return Err(anyhow!("Withdraw aborted"));
                    }

                    let pb = progress_bar_with_style(accounts.len() as u64);
                    let mut not_drained = 0;

                    accounts.iter().for_each(|account| {
                        let (tars, _account) = account;
                        do_withdraw(program.clone(), *tars, payer).unwrap_or_else(|e| {
                            not_drained += 1;
                            error!("Error: {}", e);
                        });
                        pb.inc(1);
                    });

                    pb.finish();

                    if not_drained > 0 {
                        println!(
                            "{}",
                            style(format!("Could not drain {} tars(s)", not_drained))
                                .red()
                                .bold()
                                .dim()
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

fn setup_withdraw(keypair: Option<String>, rpc_url: Option<String>) -> Result<(Program, Pubkey)> {
    let case_config = case_setup(keypair, rpc_url)?;
    let client = setup_client(&case_config)?;
    let program = client.program(TARS_ID);
    let payer = program.payer();

    Ok((program, payer))
}

fn do_withdraw(program: Rc<Program>, tars: Pubkey, payer: Pubkey) -> Result<()> {
    program
        .request()
        .accounts(nft_accounts::WithdrawFunds {
            tars,
            authority: payer,
        })
        .args(nft_instruction::WithdrawFunds {})
        .send()?;

    Ok(())
}
