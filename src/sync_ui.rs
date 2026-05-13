use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::stdout;
use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::Duration;

pub enum SyncUI {
    Pretty(PrettySyncUI),
    Plain,
}

impl SyncUI {
    pub fn new(total_files: usize) -> Self {
        if stdout().is_terminal() {
            Self::Pretty(PrettySyncUI::new(total_files))
        } else {
            Self::Plain
        }
    }

    pub fn start_scan(&self, dir: &str) {
        match self {
            SyncUI::Pretty(_) => {}
            SyncUI::Plain => tracing::info!("Scanning {dir}..."),
        }
    }

    pub fn start_file(&mut self, path: &str, file_index: usize) {
        match self {
            SyncUI::Pretty(ui) => ui.start_file(path, file_index),
            SyncUI::Plain => tracing::info!("Indexing {path}"),
        }
    }

    pub fn finish_file(&mut self, path: &str, chunks: usize) {
        match self {
            SyncUI::Pretty(ui) => ui.finish_file(path, chunks),
            SyncUI::Plain => tracing::info!("Indexed {path} ({chunks} chunks)"),
        }
    }

    pub fn finish(&mut self, total_files: usize, total_chunks: usize) {
        match self {
            SyncUI::Pretty(ui) => ui.finish(total_files, total_chunks),
            SyncUI::Plain => {
                tracing::info!("Sync complete: {total_files} files, {total_chunks} chunks indexed")
            }
        }
    }
}

pub struct PrettySyncUI {
    multi: MultiProgress,
    target_bar: ProgressBar,
    _rain_bars: Vec<ProgressBar>,
    chunks_bar: ProgressBar,
    total_files: usize,
    total_chunks: usize,
    files_started: usize,
    active_files: usize,
    shred_tick: usize,
    tx: mpsc::Sender<()>,
    rain_handle: Option<std::thread::JoinHandle<()>>,
}

impl PrettySyncUI {
    fn new(total_files: usize) -> Self {
        let multi = MultiProgress::with_draw_target(ProgressDrawTarget::stdout());

        let target_bar = multi.add(ProgressBar::new_spinner());
        let target_style = ProgressStyle::with_template("  [{prefix:.cyan}] {msg:.bold}")
            .expect("Failed to parse target progress bar template")
            .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈");
        target_bar.set_style(target_style);
        target_bar.set_prefix(" TARGET ");

        let mut rain_bars = Vec::new();
        for i in 0..3 {
            let bar = multi.add(ProgressBar::new_spinner());
            let rain_style = ProgressStyle::with_template("{msg}")
                .expect("Failed to parse rain progress bar template");
            bar.set_style(rain_style);
            if i == 0 {
                bar.set_message("           ↓  ↓  ↓  ↓  ↓");
            } else {
                bar.set_message("           0  1  0  1  0");
            }
            rain_bars.push(bar);
        }

        let spacer = multi.add(ProgressBar::new_spinner());
        spacer.set_style(
            ProgressStyle::with_template("").expect("Failed to parse spacer progress bar template"),
        );

        let chunks_bar = multi.add(ProgressBar::new(0));
        let chunks_style = ProgressStyle::with_template("  [{prefix:.cyan}] {spinner} {msg}")
            .expect("Failed to parse chunks progress bar template")
            .progress_chars("▓▓░");
        chunks_bar.set_style(chunks_style);
        chunks_bar.set_prefix(" CHUNKS ");
        chunks_bar.set_message(" waiting for work...");

        let (tx, rx) = mpsc::channel();
        let rain_bars_clone = rain_bars.clone();
        let handle = std::thread::Builder::new()
            .name("kt-rain".into())
            .spawn(move || rain_loop(rx, rain_bars_clone))
            .ok();

        Self {
            multi,
            target_bar,
            _rain_bars: rain_bars,
            chunks_bar,
            total_files,
            total_chunks: 0,
            files_started: 0,
            active_files: 0,
            shred_tick: 0,
            tx,
            rain_handle: handle,
        }
    }

    fn start_file(&mut self, _path: &str, _file_index: usize) {
        self.target_bar.set_prefix(format!(
            " ACTIVE {}/{}",
            self.active_files + 1,
            self.total_files
        ));
        self.active_files += 1;
        self.files_started += 1;
        let shred = self.shred_message();
        self.target_bar.set_message(shred);
        self.chunks_bar.set_message(self.status_line("shredding"));
        self.target_bar.tick();
        self.chunks_bar.tick();
    }

    fn finish_file(&mut self, _path: &str, chunks: usize) {
        self.total_chunks += chunks;
        self.active_files = self.active_files.saturating_sub(1);
        let _ = _path;
        let shred = self.shred_message();
        self.target_bar.set_message(shred);
        self.chunks_bar.set_message(self.status_line("processed"));
        self.target_bar.set_prefix(format!(
            " ACTIVE {}/{}",
            self.active_files, self.total_files
        ));
        self.chunks_bar.tick();
        self.target_bar.tick();
    }

    fn finish(&mut self, total_files: usize, total_chunks: usize) {
        let _ = self.tx.send(());
        if let Some(handle) = self.rain_handle.take() {
            let _ = handle.join();
        }

        self.multi.clear().ok();

        let width = 51;
        let top = format_box_top(width);
        let bot = format_box_bot(width);
        let line1 = format_box_line(&format!("  {} SYNC COMPLETE", style("✓").green()), width);
        let detail = format_sync_summary(total_files, total_chunks);
        let line2 = format_box_line(&detail, width);

        println!("\n{top}\n{line1}\n{line2}\n{bot}\n");
    }

    fn status_line(&self, state: &str) -> String {
        format!(
            "{} {} | {} | {}",
            state,
            style(format!("{} chunks", self.total_chunks)).green(),
            style(format!("{} active files", self.active_files)).cyan(),
            style(format!("{} files seen", self.files_started)).blue(),
        )
    }

    fn shred_message(&mut self) -> String {
        let dots = match self.shred_tick % 3 {
            0 => ".",
            1 => "..",
            2 => "...",
            _ => ".",
        };
        self.shred_tick += 1;
        format!("{}{}", style("shredding").yellow(), style(dots).green())
    }
}

fn format_sync_summary(total_files: usize, total_chunks: usize) -> String {
    format!(
        "    {} files shredded into {} chunks",
        total_files, total_chunks
    )
}

fn rain_loop(rx: mpsc::Receiver<()>, bars: Vec<ProgressBar>) {
    loop {
        if rx.try_recv().is_ok() {
            break;
        }

        bars[0].set_message("           ↓  ↓  ↓  ↓  ↓");
        bars[0].tick();

        for bar in bars.iter().skip(1) {
            let mut bits = String::with_capacity(20);
            bits.push_str("           ");
            for i in 0..5 {
                let bit = if fastrand::bool() { "1" } else { "0" };
                let colored = if fastrand::bool() {
                    style(bit).green().to_string()
                } else {
                    style(bit).cyan().to_string()
                };
                bits.push_str(&colored);
                if i < 4 {
                    bits.push_str("  ");
                }
            }
            bar.set_message(bits);
            bar.tick();
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn format_box_top(width: usize) -> String {
    format!(
        "  {}{}{}",
        style("|=").cyan(),
        style("=".repeat(width)).cyan(),
        style("=|").cyan()
    )
}

fn format_box_bot(width: usize) -> String {
    format!(
        "  {}{}{}",
        style("|=").cyan(),
        style("=".repeat(width)).cyan(),
        style("=|").cyan()
    )
}

fn format_box_line(content: &str, width: usize) -> String {
    let stripped = console::strip_ansi_codes(content).len();
    let padding = width.saturating_sub(stripped);
    format!(
        "  {}{}{}{}",
        style("| ").cyan(),
        content,
        " ".repeat(padding),
        style(" |").cyan()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_ui_start_and_finish_file_track_aggregate_progress() {
        let mut ui = PrettySyncUI::new(3);
        ui.start_file("alpha.rs", 0);
        ui.start_file("beta.rs", 1);
        ui.finish_file("alpha.rs", 10);

        assert_eq!(ui.files_started, 2);
        assert_eq!(ui.active_files, 1);
        assert_eq!(ui.total_chunks, 10);

        ui.start_file("gamma.rs", 2);
        ui.finish_file("beta.rs", 7);
        ui.finish_file("gamma.rs", 5);

        assert_eq!(ui.files_started, 3);
        assert_eq!(ui.active_files, 0);
        assert_eq!(ui.total_chunks, 22);

        assert_eq!(
            format_sync_summary(3, 22),
            "    3 files shredded into 22 chunks"
        );
        ui.finish(3, 22);
    }
}
