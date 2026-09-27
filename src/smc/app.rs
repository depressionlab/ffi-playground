use std::collections::VecDeque;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::control::{Cmd, Controller, Note};
use super::fan::{self, Fan, FanMode};
use super::smc::{Smc, is_root};
use super::temps::Temps;

pub const HISTORY_LEN: usize = 240;

pub struct Status {
	pub error: bool,
	pub text: String,
}

pub struct App {
	pub smc: Smc,
	pub fans: Vec<Fan>,
	pub desired: Vec<Option<f32>>,
	pub temps: Temps,
	pub model: String,
	pub selected: usize,
	pub linked: bool,
	pub is_root: bool,
	pub status: Option<Status>,
	pub history: Vec<VecDeque<u64>>,
	controller: Controller,
	keep_on_exit: bool,
	needs_refresh: bool,
	last_refresh: Instant,
	last_history: Instant,
}

impl App {
	pub fn new(
		smc: Smc,
		fans: Vec<Fan>,
		temps: Temps,
		model: String,
		controller: Controller,
	) -> Self {
		let history = fans.iter().map(|_| VecDeque::new()).collect();
		let desired = vec![None; fans.len()];
		let mut app = Self {
			smc,
			fans,
			desired,
			temps,
			model,
			selected: 0,
			linked: true,
			is_root: is_root(),
			status: None,
			history,
			controller,
			keep_on_exit: false,
			needs_refresh: false,
			last_refresh: Instant::now(),
			last_history: Instant::now(),
		};
		app.record_history();
		app
	}

	pub const fn keep_on_exit(&self) -> bool {
		self.keep_on_exit
	}

	pub fn run(&mut self, terminal: &mut DefaultTerminal) -> eyre::Result<()> {
		loop {
			if self.drain_notes() {
				eyre::bail!(
					"fan control thread stopped unexpectedly; its safety guard restored automatic control"
				);
			}
			if self.needs_refresh || self.last_refresh.elapsed() >= Duration::from_secs(1) {
				self.refresh();
				self.needs_refresh = false;
			}
			if self.last_history.elapsed() >= Duration::from_secs(1) {
				self.record_history();
				self.last_history = Instant::now();
			}
			terminal.draw(|frame| super::ui::draw(frame, self))?;
			if event::poll(Duration::from_millis(100))?
				&& let Event::Key(k) = event::read()?
				&& k.kind == KeyEventKind::Press
				&& self.on_key(k)
			{
				break;
			}
		}
		self.controller.send(if self.keep_on_exit {
			Cmd::QuitKeep
		} else {
			Cmd::QuitRestore
		});
		Ok(())
	}

	fn drain_notes(&mut self) -> bool {
		loop {
			match self.controller.rx.try_recv() {
				Ok(Note::Info(text)) => self.status = Some(Status { error: false, text }),
				Ok(Note::Error { index, text }) => {
					match index {
						Some(i) => self.desired[i] = None,
						None => self.desired.iter_mut().for_each(|d| *d = None),
					}
					self.status = Some(Status { error: true, text });
				}
				Ok(Note::Applied) => {
					self.status = None;
					self.needs_refresh = true;
				}
				Err(TryRecvError::Empty) => return false,
				Err(TryRecvError::Disconnected) => return true,
			}
		}
	}

	fn refresh(&mut self) {
		for fan in &mut self.fans {
			fan::refresh(&self.smc, fan);
		}
		self.temps.refresh(&self.smc);
		self.last_refresh = Instant::now();
	}

	fn record_history(&mut self) {
		for (i, fan) in self.fans.iter().enumerate() {
			let h = &mut self.history[i];
			h.push_back(fan.actual.max(0.0) as u64);
			while h.len() > HISTORY_LEN {
				h.pop_front();
			}
		}
	}

	fn on_key(&mut self, key: KeyEvent) -> bool {
		let shift = key.modifiers.contains(KeyModifiers::SHIFT);
		match key.code {
			KeyCode::Char('q') | KeyCode::Esc => return true,
			KeyCode::Char('Q') => {
				self.keep_on_exit = true;
				return true;
			}
			KeyCode::Up | KeyCode::Char('k') => {
				self.selected = self.selected.saturating_sub(1);
			}
			KeyCode::Down | KeyCode::Char('j') => {
				self.selected = (self.selected + 1).min(self.fans.len().saturating_sub(1));
			}
			KeyCode::Right if shift => self.bump(500.0),
			KeyCode::Left if shift => self.bump(-500.0),
			KeyCode::Right | KeyCode::Char('l' | '+' | '=') => self.bump(100.0),
			KeyCode::Left | KeyCode::Char('h' | '-') => self.bump(-100.0),
			KeyCode::Char('L') => self.bump(500.0),
			KeyCode::Char('H') => self.bump(-500.0),
			KeyCode::Char('m') => self.toggle_mode(),
			KeyCode::Char('a') => self.all_auto(),
			KeyCode::Char('f') => self.full_blast(),
			KeyCode::Char(' ') => self.linked = !self.linked,
			KeyCode::Char('r') => self.refresh(),
			_ => {}
		}
		false
	}

	fn target_indices(&self) -> Vec<usize> {
		if self.linked {
			(0..self.fans.len()).collect()
		} else {
			vec![self.selected]
		}
	}

	fn bump(&mut self, delta: f32) {
		for i in self.target_indices() {
			let fan = &self.fans[i];
			let base = self.desired[i].unwrap_or(if fan.mode == FanMode::Manual {
				fan.target
			} else {
				fan.actual
			});
			let rpm = (base + delta).clamp(fan.min.max(0.0), fan.max);
			self.desired[i] = Some(rpm);
			self.controller.send(Cmd::SetTarget { index: i, rpm });
		}
	}

	fn toggle_mode(&mut self) {
		let to_manual = self.fans[self.selected].mode != FanMode::Manual;
		for i in self.target_indices() {
			if to_manual {
				let fan = &self.fans[i];
				let rpm = self.desired[i].unwrap_or_else(|| fan.actual.max(fan.min));
				self.desired[i] = Some(rpm);
				self.controller.send(Cmd::SetTarget { index: i, rpm });
			} else {
				self.desired[i] = None;
				self.controller.send(Cmd::SetAuto { index: i });
			}
		}
	}

	fn all_auto(&mut self) {
		self.desired.iter_mut().for_each(|d| *d = None);
		self.controller.send(Cmd::AllAuto);
	}

	fn full_blast(&mut self) {
		for i in self.target_indices() {
			let rpm = self.fans[i].max;
			self.desired[i] = Some(rpm);
			self.controller.send(Cmd::SetTarget { index: i, rpm });
		}
	}
}
