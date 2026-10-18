use crate::Result;
use std::{fs, path::PathBuf};

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
pub fn percentile(v: &[f64], p: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * p).ceil() as usize)
        .saturating_sub(1)
        .min(v.len() - 1)]
}
pub struct Output {
    pub json: Option<PathBuf>,
    pub rows: Vec<String>,
    pub md: String,
    pub machine: String,
}
impl Output {
    pub fn save(&self) -> Result<()> {
        if let Some(p) = &self.json {
            if let Some(parent) = p.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)?;
            }
            let temp = p.with_extension("json.tmp");
            fs::write(
                &temp,
                format!(
                    "{{\"schema\":1,\"machine\":{},\"measurements\":[\n{}\n]}}\n",
                    quote(&self.machine),
                    self.rows.join(",\n")
                ),
            )?;
            fs::rename(temp, p)?;
            fs::write(p.with_extension("md"), &self.md)?;
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn row(
        &mut self,
        n: usize,
        bytes: u64,
        store: &str,
        op: &str,
        unit: &str,
        values: &[f64],
        note: &str,
    ) -> Result<()> {
        let (median, p95, p99) = if values.is_empty() {
            (None, None, None)
        } else {
            (
                Some(percentile(values, 0.5)),
                Some(percentile(values, 0.95)),
                Some(percentile(values, 0.99)),
            )
        };
        let num = |n: Option<f64>| n.map_or("null".into(), |v| format!("{v:.6}"));
        self.rows.push(format!("{{\"files\":{n},\"corpus_bytes\":{bytes},\"store\":{},\"operation\":{},\"unit\":{},\"samples\":{:?},\"median\":{},\"p95\":{},\"p99\":{},\"note\":{}}}", quote(store),quote(op),quote(unit),values,num(median),num(p95),num(p99),quote(note)));
        let display = |n: Option<f64>| n.map_or("—".into(), |v| format!("{v:.3}"));
        let line = format!(
            "| {n} | {bytes} | {store} | {op} | {unit} | {} | {} | {} | {} | {note} |\n",
            values.len(),
            display(median),
            display(p95),
            display(p99)
        );
        print!("{line}");
        self.md.push_str(&line);
        self.save()
    }
}
