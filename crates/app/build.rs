use slint_build::{CompilerConfiguration, DefaultTranslationContext};

fn main() {
    // Traductions gettext embarquées : lang/<langue>/LC_MESSAGES/audioplayer.po
    let config = CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("lang")
        .with_default_translation_context(DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint compilation failed");
}
