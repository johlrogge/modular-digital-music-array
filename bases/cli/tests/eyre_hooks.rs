#[test]
fn init_installs_color_eyre_hooks() {
    cli_base::init();
    assert!(color_eyre::install().is_err());
}
