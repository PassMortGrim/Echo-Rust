//! TuiApp — dashboard terminal temps réel avec ratatui + crossterm.

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use crate::state::graph::{AgentState, Phase};

pub struct TuiApp {
    state: Arc<Mutex<AgentState>>,
    logs: Arc<Mutex<Vec<String>>>,
}

impl TuiApp {
    pub fn new(state: Arc<Mutex<AgentState>>) -> Self {
        Self {
            state,
            logs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn push_log(&self, msg: String) {
        let mut logs = self.logs.lock().unwrap();
        logs.push(msg);
        if logs.len() > 200 {
            logs.remove(0);
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        loop {
            terminal.draw(|f| self.draw(f))?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        _ => {}
                    }
                }
            }

            // Vérifier si l'agent a terminé
            let phase = self.state.lock().unwrap().phase.clone();
            if matches!(phase, Phase::Done | Phase::Failed(_)) {
                break;
            }
        }

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;
        Ok(())
    }

    // ── Rendu ─────────────────────────────────────────────────────────

    fn draw(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(3),  // header
                Constraint::Length(8),  // état agent
                Constraint::Min(4),     // logs
            ])
            // .split(f.area()); -> Dans ratatui 0.26, Frame expose size() ; area() a été introduit dans une version ultérieure.
            .split(f.size());

        // Header
        let header = Paragraph::new(Line::from(vec![
            Span::styled(
                " Echo-Rust ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("— Automated Bug Reproduction Agent"),
        ]))
        .block(Block::default().borders(Borders::ALL));
        f.render_widget(header, chunks[0]);

        // État de l'agent
        let st = self.state.lock().unwrap();
        let phase_color = match st.phase {
            Phase::Done      => Color::Green,
            Phase::Failed(_) => Color::Red,
            _                => Color::Yellow,
        };

        let info = vec![
            Line::from(vec![
                Span::styled(" Issue  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(&st.issue),
            ]),
            Line::from(vec![
                Span::styled(" Repo   ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(&st.repo_url),
            ]),
            Line::from(vec![
                Span::styled(" Phase  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(st.phase.to_string(), Style::default().fg(phase_color)),
            ]),
            Line::from(vec![
                Span::styled(" Iter   ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{}/{}", st.iteration, 5)),
            ]),
            Line::from(vec![
                Span::styled(" AST    ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{} nœuds / {} arêtes", st.ast_nodes, st.graph_edges)),
            ]),
            Line::from(vec![
                Span::styled(" Script ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(if st.reproduction_script.is_some() {
                    "✓ généré"
                } else {
                    "… en attente"
                }),
            ]),
        ];

        let panel = Paragraph::new(info)
            .block(Block::default().borders(Borders::ALL).title(" Agent State "))
            .wrap(Wrap { trim: true });
        f.render_widget(panel, chunks[1]);

        // Logs
        let logs = self.logs.lock().unwrap();
        let items: Vec<ListItem> = logs
            .iter()
            .rev()
            .take(chunks[2].height as usize)
            .map(|l| ListItem::new(l.as_str()))
            .collect();

        let log_panel = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Logs (q pour quitter) "));
        f.render_widget(log_panel, chunks[2]);
    }
}