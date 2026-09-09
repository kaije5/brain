use brain::{Cli, Command, TaskCommand};
use clap::Parser;

#[test]
fn task_add_parses_due_and_json_output() {
    let cli = Cli::try_parse_from([
        "brain",
        "--output",
        "json",
        "task",
        "add",
        "Finish architecture",
        "--due",
        "2026-09-01",
    ])
    .expect("CLI parses");
    assert!(matches!(
        cli.command,
        Command::Task(TaskCommand::Add { .. })
    ));
}

#[test]
fn every_top_level_command_accepts_json_output() {
    let cli = Cli::try_parse_from(["brain", "--output", "json", "status"]).expect("parses");
    assert!(matches!(cli.command, Command::Status));
}
