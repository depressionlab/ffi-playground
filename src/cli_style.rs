use clap::builder::styling::{AnsiColor, Effects, Styles};

/// Clap help styling: bold cyan headers/usage, bold green literals (flag and
/// subcommand names), plain placeholders, bold red errors.
pub fn help_styles() -> Styles {
	Styles::styled()
		.header(AnsiColor::Cyan.on_default() | Effects::BOLD)
		.header(AnsiColor::Cyan.on_default() | Effects::BOLD)
		.usage(AnsiColor::Cyan.on_default() | Effects::BOLD)
		.literal(AnsiColor::Green.on_default() | Effects::BOLD)
		.placeholder(AnsiColor::White.on_default())
		.error(AnsiColor::Red.on_default() | Effects::BOLD)
		.valid(AnsiColor::Green.on_default())
		.invalid(AnsiColor::Yellow.on_default())
}
