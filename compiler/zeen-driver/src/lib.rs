mod span;

pub use span::LineOffsets;
pub use span::LocationSpan;

pub struct MietteDriver {
    reporter: miette::GraphicalReportHandler,
}

impl MietteDriver {
    pub fn new() -> Self {
        let reporter = miette::GraphicalReportHandler::new()
            .tab_width(2)
            .with_links(true)
            .with_cause_chain();

        Self { reporter }
    }

    pub fn report(&self, diagnostic: &dyn miette::Diagnostic) -> Result<String, std::fmt::Error> {
        let mut buffer = String::new();

        self.reporter.render_report(&mut buffer, diagnostic)?;

        return Ok(buffer);
    }
}
