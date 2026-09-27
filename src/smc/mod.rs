mod app;
mod control;
mod fan;
mod smc;
mod temps;
mod ui;

fn model_name() -> String {
	let chip = std::process::Command::new("sysctl")
		.args(["-n", "machdep.cpu.brand_string"])
		.output()
		.ok()
		.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
		.filter(|s| !s.is_empty());
	let model = std::process::Command::new("sysctl")
		.args(["-n", "hw.model"])
		.output()
		.ok()
		.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
		.filter(|s| !s.is_empty())
		.unwrap_or_else(|| "Mac".into());
	match chip {
		Some(chip) => format!("{model} · {chip}"),
		None => model,
	}
}

pub fn run(list: bool, auto: bool) -> eyre::Result<()> {
	let smc = smc::Smc::open()?;
	let fans = fan::discover(&smc)?;
	if fans.is_empty() {
		eyre::bail!("no fans found (is this a fanless Mac?)");
	}

	if auto {
		if !smc::is_root() {
			eyre::bail!("restoring automatic fan control requires root!");
		}
		let problems = control::force_auto(&smc, &fans);
		if problems.is_empty() {
			println!("all fans restored to automatic control");
			return Ok(());
		}
		eyre::bail!(problems.join("; "));
	}

	if list {
		println!("discovering sensors...");
		let temps = temps::Temps::discover(&smc);
		println!("{}", model_name());
		println!();
		for fan in &fans {
			let mode = match fan.mode {
				fan::FanMode::Manual => "MANUAL",
				fan::FanMode::System => "SYSTEM",
				fan::FanMode::Auto => "AUTO",
			};
			println!(
				"  {:<12} {:>5.0} RPM  target {:>5.0}  range {:.0}-{:.0}  [{mode}]",
				fan.name, fan.actual, fan.target, fan.min, fan.max,
			);
		}
		println!();
		match &temps.hottest {
			Some((key, t)) => println!(
				"  temps: avg {:.1}°C, hottest {t:.1}°C ({key}), {} sensors",
				temps.avg,
				temps.sensor_count(),
			),
			None => println!("  temps: no readable sensors"),
		}
		return Ok(());
	}

	let temps = temps::Temps::discover(&smc);
	let controller = control::Controller::spawn(fans.clone())?;
	let mut terminal = ratatui::init();
	let mut app = app::App::new(smc, fans, temps, model_name(), controller);
	let result = app.run(&mut terminal);
	ratatui::restore();
	if app.keep_on_exit() {
		println!("keeping manual fan settings, use `--auto` to restore");
	} else {
		println!("restoring fans to automatic control...");
	}
	use std::io::Write;
	let _ = std::io::stdout().flush();
	drop(app);
	result
}
