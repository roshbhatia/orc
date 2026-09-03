use minijinja::Environment;

pub fn render_fixture(template: &str, values: serde_json::Value) -> String {
    let mut environment = Environment::new();
    environment
        .add_template("fixture", template)
        .expect("fixture template");
    environment
        .get_template("fixture")
        .expect("fixture template")
        .render(values)
        .expect("render fixture")
}
