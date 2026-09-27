use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Gauge, Paragraph, Sparkline};

use super::app::App;
use super::fan::{Fan, FanMode};

const ACCENT: Color = Color::Rgb(0, 215, 255);
const HOT: Color = Color::Rgb(255, 95, 95);
const WARM: Color = Color::Rgb(255, 215, 0);
const COOL: Color = Color::Rgb(0, 255, 135);
const DIM: Color = Color::Rgb(110, 110, 130);
const MANUAL: Color = Color::Rgb(255, 121, 198);

fn rpm_color(ratio: f64) -> Color {
	if ratio < 0.45 {
		COOL
	} else if ratio < 0.75 {
		WARM
	} else {
		HOT
	}
}

fn temp_color(t: f32) -> Color {
	if t < 60.0 {
		COOL
	} else if t < 85.0 {
		WARM
	} else {
		HOT
	}
}

pub fn draw(frame: &mut Frame<'_>, app: &App) {
	let mut constraints = vec![Constraint::Length(1)];
	constraints.extend(app.fans.iter().map(|_| Constraint::Length(4)));
	constraints.push(Constraint::Length(3));
	constraints.push(Constraint::Min(4));
	constraints.push(Constraint::Length(1));
	constraints.push(Constraint::Length(1));
	let areas = Layout::vertical(constraints).split(frame.area());

	draw_title(frame, areas[0], app);
	for (i, fan) in app.fans.iter().enumerate() {
		draw_fan(
			frame,
			areas[1 + i],
			fan,
			app.desired[i],
			i == app.selected,
			app.linked,
		);
	}
	let base = 1 + app.fans.len();
	draw_temps(frame, areas[base], app);
	draw_sparkline(frame, areas[base + 1], app);
	draw_status(frame, areas[base + 2], app);
	draw_help(frame, areas[base + 3]);
}

fn draw_title(frame: &mut Frame<'_>, area: Rect, app: &App) {
	let access = if app.is_root {
		Span::styled(" CONTROL ", Style::new().fg(Color::Black).bg(COOL).bold())
	} else {
		Span::styled(" READ-ONLY ", Style::new().fg(Color::Black).bg(WARM).bold())
	};
	let line = Line::from(vec![
		Span::styled(
			" FFI-PLAYGROUND ",
			Style::new().fg(Color::Black).bg(ACCENT).bold(),
		),
		Span::raw(" "),
		Span::styled(&app.model, Style::new().fg(Color::White).bold()),
		Span::raw("  "),
		access,
	]);
	frame.render_widget(Paragraph::new(line), area);
}

fn mode_badge(mode: FanMode) -> Span<'static> {
	match mode {
		FanMode::Manual => {
			Span::styled(" MANUAL ", Style::new().fg(Color::Black).bg(MANUAL).bold())
		}
		FanMode::System => Span::styled(
			" SYSTEM ",
			Style::new()
				.fg(Color::Black)
				.bg(Color::Rgb(150, 150, 255))
				.bold(),
		),
		FanMode::Auto => Span::styled(" AUTO ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
	}
}

fn draw_fan(
	frame: &mut Frame<'_>,
	area: Rect,
	fan: &Fan,
	desired: Option<f32>,
	selected: bool,
	linked: bool,
) {
	let marker = if selected { "▶ " } else { "  " };
	let link = if selected && linked {
		Span::styled(" linked ", Style::new().fg(DIM).italic())
	} else {
		Span::raw("")
	};
	let border_style = if selected {
		Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
	} else {
		Style::new().fg(DIM)
	};
	let block = Block::bordered()
		.border_type(if selected {
			BorderType::Thick
		} else {
			BorderType::Rounded
		})
		.border_style(border_style)
		.title(Line::from(vec![
			Span::styled(
				format!("{marker}{} ", fan.name),
				if selected {
					Style::new().fg(ACCENT).bold()
				} else {
					Style::new().fg(Color::White)
				},
			),
			mode_badge(fan.mode),
			link,
		]));
	let inner = block.inner(area);
	frame.render_widget(block, area);

	let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

	let mut spans = vec![
		Span::styled(
			format!("{:>5.0}", fan.actual),
			Style::new().fg(Color::White).bold(),
		),
		Span::styled(" RPM", Style::new().fg(DIM)),
	];
	if fan.mode == FanMode::Manual {
		spans.push(Span::styled(
			format!("  target {:>5.0}", fan.target),
			Style::new().fg(MANUAL).bold(),
		));
	} else {
		spans.push(Span::styled(
			format!("  target {:>5.0}", fan.target),
			Style::new().fg(DIM),
		));
	}
	if let Some(d) = desired
		&& (d - fan.target).abs() > 1.0
	{
		spans.push(Span::styled(
			format!(" → {d:.0}"),
			Style::new().fg(WARM).bold(),
		));
	}
	spans.push(Span::styled(
		format!("    min {:.0} · max {:.0}", fan.min, fan.max),
		Style::new().fg(DIM),
	));
	frame.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

	let span = (fan.max - fan.min).max(1.0);
	let raw = f64::from((fan.actual - fan.min) / span);
	let ratio = if raw.is_finite() {
		raw.clamp(0.0, 1.0)
	} else {
		0.0
	};
	let gauge = Gauge::default()
		.ratio(ratio)
		.label(Span::styled(
			format!("{:>4.0} RPM", fan.actual),
			Style::new().fg(Color::White).bold(),
		))
		.gauge_style(Style::new().fg(rpm_color(ratio)).bg(Color::Rgb(30, 30, 40)));
	frame.render_widget(gauge, rows[1]);
}

fn draw_temps(frame: &mut Frame<'_>, area: Rect, app: &App) {
	let block = Block::bordered()
		.border_type(BorderType::Rounded)
		.border_style(Style::new().fg(DIM))
		.title(Span::styled(
			" thermals ",
			Style::new().fg(Color::White).bold(),
		));
	let inner = block.inner(area);
	frame.render_widget(block, area);

	let Some((key, t)) = &app.temps.hottest else {
		frame.render_widget(
			Paragraph::new(Line::from(Span::styled(
				"no readable temperature sensors",
				Style::new().fg(DIM).italic(),
			))),
			inner,
		);
		return;
	};
	let mut spans = vec![
		Span::styled("avg ", Style::new().fg(DIM)),
		Span::styled(
			format!("{:.1}°C", app.temps.avg),
			Style::new().fg(temp_color(app.temps.avg)).bold(),
		),
		Span::styled("   hottest ", Style::new().fg(DIM)),
		Span::styled(format!("{t:.1}°C"), Style::new().fg(temp_color(*t)).bold()),
		Span::styled(format!(" ({key})"), Style::new().fg(DIM)),
	];
	spans.push(Span::styled(
		format!("   {} sensors", app.temps.sensor_count()),
		Style::new().fg(DIM),
	));
	frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_sparkline(frame: &mut Frame<'_>, area: Rect, app: &App) {
	let fan = &app.fans[app.selected];
	let history = &app.history[app.selected];
	let block = Block::bordered()
		.border_type(BorderType::Rounded)
		.border_style(Style::new().fg(DIM))
		.title(Span::styled(
			format!(" {} rpm history ", fan.name),
			Style::new().fg(Color::White).bold(),
		));
	let inner = block.inner(area);
	frame.render_widget(block, area);

	let width = inner.width as usize;
	let skip = history.len().saturating_sub(width);
	let data: Vec<u64> = history.iter().copied().skip(skip).collect();
	let spark = Sparkline::default()
		.data(data)
		.max(fan.max.max(1.0) as u64)
		.style(Style::new().fg(ACCENT));
	frame.render_widget(spark, inner);
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
	let line = app.status.as_ref().map_or_else(
		|| {
			if app.is_root {
				Line::from(Span::styled(
					" fans restore to auto on quit (q): use Q to keep settings",
					Style::new().fg(DIM),
				))
			} else {
				Line::from(Span::styled(
					" read-only mode: run using `sudo` to control fans",
					Style::new().fg(WARM),
				))
			}
		},
		|status| {
			if status.error {
				Line::from(Span::styled(
					format!(" ⚠ {}", status.text),
					Style::new().fg(HOT).bold(),
				))
			} else {
				Line::from(Span::styled(
					format!(" ◌ {}", status.text),
					Style::new().fg(WARM),
				))
			}
		},
	);
	frame.render_widget(Paragraph::new(line), area);
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
	let help = Line::from(Span::styled(
		" ↑↓ select   ←→ ±100   ⇧←→ ±500   m manual/auto   a all auto   f full blast   space link   q quit·restore   Q quit·keep",
		Style::new().fg(DIM),
	));
	frame.render_widget(Paragraph::new(help), area);
}
