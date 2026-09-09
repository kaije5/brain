use brain::{Cli, Command, ConfigCommand, SecretCommand};
use clap::Parser;

#[test]
fn config_init_parses_as_a_local_command() {
    let cli = Cli::try_parse_from(["brain", "config", "init"]).expect("valid invocation");
    assert!(matches!(
        cli.command.as_ref(),
        Some(Command::Config(ConfigCommand::Init))
    ));
}

#[test]
fn secret_import_parses_with_a_named_profile() {
    let cli = Cli::try_parse_from(["brain", "secret", "import", "--profile", "nim"])
        .expect("valid invocation");
    match cli.command.as_ref() {
        Some(Command::Secret(SecretCommand::Import { profile })) => assert_eq!(profile, "nim"),
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn secret_import_requires_a_profile_argument() {
    assert!(Cli::try_parse_from(["brain", "secret", "import"]).is_err());
}
