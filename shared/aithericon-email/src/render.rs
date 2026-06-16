//! Tera rendering: templates are embedded into the binary at compile time
//! (`include_str!`), so there is zero runtime file I/O and templates can never
//! drift from the deployed code.
//!
//! A single [`Renderer`] is built once at startup (carrying the deployment
//! [`Branding`]) and shared by reference into whichever adapter is active.

use tera::{Context, Tera};

use crate::config::Branding;
use crate::error::{EmailError, Result};
use crate::port::TemplateMessage;

/// `(tera name, source)` for every template embedded in the crate. The base
/// layout/partials must be registered so `{% extends %}` / `{% include %}`
/// resolve.
fn embedded_templates() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "base/layout.html",
            include_str!("../templates/base/layout.html"),
        ),
        (
            "base/header.html",
            include_str!("../templates/base/header.html"),
        ),
        (
            "base/footer.html",
            include_str!("../templates/base/footer.html"),
        ),
        (
            "workspace_invite.html",
            include_str!("../templates/workspace_invite.html"),
        ),
        (
            "resource_shared.html",
            include_str!("../templates/resource_shared.html"),
        ),
        (
            "member_added.html",
            include_str!("../templates/member_added.html"),
        ),
        ("welcome.html", include_str!("../templates/welcome.html")),
    ]
}

/// A rendered email ready for transport.
#[derive(Debug, Clone)]
pub struct RenderedEmail {
    pub subject: String,
    pub html: String,
}

/// Owns the Tera engine and the deployment branding. Cheap to share via `Arc`.
pub struct Renderer {
    tera: Tera,
    branding: Branding,
}

impl Renderer {
    /// Build the engine and register every embedded template. Fails only on a
    /// genuine template syntax error (i.e. a bug), surfaced at startup.
    pub fn new(branding: Branding) -> Result<Self> {
        let mut tera = Tera::default();
        tera.add_raw_templates(embedded_templates())
            .map_err(|e| EmailError::Render(format!("loading embedded templates: {e}")))?;
        // Fail fast on undefined variables rather than silently emitting blanks.
        tera.autoescape_on(vec![".html"]);
        Ok(Self { tera, branding })
    }

    pub fn branding(&self) -> &Branding {
        &self.branding
    }

    /// Render a typed message to its subject + HTML body, merging the shared
    /// branding context underneath the message's own variables.
    pub fn render(&self, message: &dyn TemplateMessage) -> Result<RenderedEmail> {
        let mut ctx = self.branding_context(message.locale());
        ctx.extend(message.context());

        let template = format!("{}.html", message.template());
        let html = self
            .tera
            .render(&template, &ctx)
            .map_err(|e| EmailError::Render(format!("{template}: {}", render_chain(&e))))?;

        Ok(RenderedEmail {
            subject: message.subject(),
            html,
        })
    }

    /// The branding/boilerplate keys every template can rely on.
    fn branding_context(&self, locale: &str) -> Context {
        let mut ctx = Context::new();
        ctx.insert("product_name", &self.branding.product_name);
        ctx.insert("base_url", &self.branding.base_url);
        ctx.insert("support_address", &self.branding.support_address);
        ctx.insert("current_year", &chrono::Utc::now().format("%Y").to_string());
        ctx.insert("lang", locale);
        ctx
    }
}

/// Tera nests the real cause in `.source()`; flatten it for a useful log line.
fn render_chain(e: &tera::Error) -> String {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    msg
}
