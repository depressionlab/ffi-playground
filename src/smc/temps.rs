use super::smc::Smc;

pub struct Temps {
	keys: Vec<String>,
	pub avg: f32,
	pub hottest: Option<(String, f32)>,
}

fn plausible(t: f32) -> bool {
	(15.0..=115.0).contains(&t)
}

impl Temps {
	pub fn discover(smc: &Smc) -> Self {
		let mut keys = Vec::new();
		if let Ok(count) = smc.key_count() {
			for i in 0..count {
				let Ok(k) = smc.key_at(i) else { continue };
				if !k.starts_with('T') {
					continue;
				}
				let Ok(v) = smc.read(&k) else { continue };
				if matches!(v.type_str().as_str(), "flt " | "sp78")
					&& v.as_f32().is_some_and(plausible)
				{
					keys.push(k);
				}
			}
		}
		let mut temps = Self {
			keys,
			avg: 0.0,
			hottest: None,
		};
		temps.refresh(smc);
		temps
	}

	pub const fn sensor_count(&self) -> usize {
		self.keys.len()
	}

	pub fn refresh(&mut self, smc: &Smc) {
		let mut sum = 0.0f32;
		let mut n = 0usize;
		let mut hottest: Option<(String, f32)> = None;
		for k in &self.keys {
			let Some(t) = smc.read(k).ok().and_then(|v| v.as_f32()) else {
				continue;
			};
			if !plausible(t) {
				continue;
			}
			sum += t;
			n += 1;
			if hottest.as_ref().is_none_or(|(_, h)| t > *h) {
				hottest = Some((k.clone(), t));
			}
		}
		self.avg = if n > 0 { sum / n as f32 } else { 0.0 };
		self.hottest = hottest;
	}
}
