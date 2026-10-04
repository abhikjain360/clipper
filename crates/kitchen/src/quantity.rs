use std::fmt::Write;

use crate::{Ingredient, Recipe, references};

const FRACTIONS: [(f64, &str); 5] = [
    (0.25, "¼"),
    (1.0 / 3.0, "⅓"),
    (0.5, "½"),
    (2.0 / 3.0, "⅔"),
    (0.75, "¾"),
];

pub fn scale_factor(recipe_servings: u32, servings: u32) -> f64 {
    if recipe_servings == 0 {
        return 1.0;
    }
    f64::from(servings) / f64::from(recipe_servings)
}

impl Ingredient {
    pub fn scaled_amount(&self, factor: f64) -> Option<f64> {
        self.amount
            .map(|amount| if self.scales { amount * factor } else { amount })
    }

    pub fn quantity(&self, factor: f64) -> String {
        format_quantity(self.scaled_amount(factor), self.unit.as_deref())
    }
}

impl Recipe {
    pub fn step_text(&self, step: usize, factor: f64) -> String {
        let Some(step) = self.steps.get(step) else {
            return String::new();
        };
        let mut text = String::with_capacity(step.text.len());
        let mut copied = 0;
        for (range, id) in references(&step.text) {
            text.push_str(&step.text[copied..range.start]);
            match self
                .ingredients
                .iter()
                .find(|ingredient| ingredient.id == id)
            {
                Some(ingredient) => {
                    let quantity = ingredient.quantity(factor);
                    if quantity.is_empty() {
                        text.push_str(&ingredient.name);
                    } else {
                        let _ = write!(text, "{} ({quantity})", ingredient.name);
                    }
                }
                None => text.push_str(&step.text[range.clone()]),
            }
            copied = range.end;
        }
        text.push_str(&step.text[copied..]);
        text
    }
}

pub fn format_quantity(amount: Option<f64>, unit: Option<&str>) -> String {
    let Some(amount) = amount else {
        return unit.unwrap_or_default().to_string();
    };
    let (amount, unit) = match unit {
        Some("g") if amount >= 1000.0 => (amount / 1000.0, Some("kg")),
        Some("ml") if amount >= 1000.0 => (amount / 1000.0, Some("l")),
        unit => (amount, unit),
    };
    let number = match unit {
        Some("g" | "ml") => rounded(amount, if amount >= 10.0 { 0 } else { 1 }),
        Some("kg" | "l") => rounded(amount, 2),
        _ => with_fraction(amount),
    };
    match unit {
        Some(unit) => format!("{number} {unit}"),
        None => number,
    }
}

fn rounded(amount: f64, decimals: i32) -> String {
    let scale = 10_f64.powi(decimals);
    plain_number((amount * scale).round() / scale)
}

fn plain_number(number: f64) -> String {
    let text = format!("{number:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn with_fraction(amount: f64) -> String {
    let whole = amount.floor();
    let rest = amount - whole;
    if rest < 0.05 {
        return plain_number(whole);
    }
    if rest > 0.95 {
        return plain_number(whole + 1.0);
    }
    match FRACTIONS
        .iter()
        .find(|(value, _)| (rest - value).abs() < 0.04)
    {
        Some((_, glyph)) if whole == 0.0 => (*glyph).to_string(),
        Some((_, glyph)) => format!("{}{glyph}", plain_number(whole)),
        None => rounded(amount, 2),
    }
}
