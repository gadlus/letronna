//! Save a session as one web page to share: what you asked and what came
//! back, styled, with nothing loaded from anywhere. Tool calls show only as
//! a count. Self-contained: removing this file, its `mod` line, its menu
//! item and the `markdown` dependency removes the feature.

use crate::prelude::*;
use letronna_core::chat::Message;
use std::path::PathBuf;

/// `text` with the five characters HTML reads escaped.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The page for a session called `title`. Your messages and the replies
/// are Markdown made into HTML, any HTML in them shown as text; the steps
/// between are counted, not shown.
pub(crate) fn page(title: &str, messages: &[Message]) -> String {
    let mut body = String::new();
    let mut steps = 0usize;
    let flush = |body: &mut String, steps: &mut usize| {
        if *steps > 0 {
            let s = if *steps == 1 { "" } else { "s" };
            body.push_str(&format!("<p class=\"steps\">{steps} step{s}</p>\n"));
            *steps = 0;
        }
    };
    for m in messages {
        if m.role == "tool" {
            steps += 1;
            continue;
        }
        if m.notice
            || m.content.trim().is_empty()
            || !matches!(m.role.as_str(), "user" | "assistant")
        {
            continue;
        }
        flush(&mut body, &mut steps);
        let (class, who) = if m.role == "user" {
            ("you", "You".to_owned())
        } else if m.model.is_empty() {
            ("reply", "Letronna".to_owned())
        } else {
            ("reply", m.model.clone())
        };
        body.push_str(&format!(
            "<section class=\"{class}\"><p class=\"who\">{}</p>{}</section>\n",
            escape(&who),
            markdown::to_html(&letronna_core::redact::mask(&m.content))
        ));
    }
    flush(&mut body, &mut steps);
    let title = escape(title);
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>
:root{{--bg:#f7f6f2;--fg:#1c1d22;--muted:#6b6f7a;--card:#fff;--line:#e3e1da;--you:#e9edfb;--accent:#1b36b8;--code:#f0efe9}}
@media (prefers-color-scheme:dark){{:root{{--bg:#11131a;--fg:#e6e7ec;--muted:#8b90a0;--card:#181b24;--line:#262a36;--you:#1a2140;--accent:#7d95ff;--code:#0d0f15}}}}
*{{box-sizing:border-box}}
body{{margin:0;background:var(--bg);color:var(--fg);font:16px/1.65 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif}}
main{{max-width:760px;margin:0 auto;padding:56px 20px 40px}}
h1.title{{font-family:"Bodoni Moda","Didot","Bodoni 72",Georgia,serif;font-weight:500;font-size:clamp(28px,5vw,40px);line-height:1.15;margin:0 0 36px}}
section{{margin:0 0 18px;padding:16px 20px;border-radius:14px;background:var(--card);border:1px solid var(--line);overflow-wrap:anywhere}}
section.you{{background:var(--you);border-color:transparent;margin-left:12%}}
.who{{margin:0 0 6px;font-size:12px;font-weight:600;letter-spacing:.04em;text-transform:uppercase;color:var(--muted)}}
section>p:last-child{{margin-bottom:0}}
.steps{{margin:-6px 0 18px 20px;font-size:13px;color:var(--muted)}}
a{{color:var(--accent)}}
code{{font:13.5px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:var(--code);padding:1px 5px;border-radius:5px}}
pre{{background:var(--code);padding:14px 16px;border-radius:10px;overflow-x:auto}}
pre code{{padding:0;background:none}}
table{{border-collapse:collapse}}td,th{{border:1px solid var(--line);padding:4px 10px}}
footer{{max-width:760px;margin:0 auto;padding:8px 20px 48px;display:flex;align-items:center;gap:8px;color:var(--muted);font-size:13px}}
footer b{{font-family:"Bodoni Moda","Didot","Bodoni 72",Georgia,serif;font-weight:600;font-size:15px;color:var(--accent)}}
</style></head>
<body><main><h1 class="title">{title}</h1>
{body}</main>
<footer>Shared from <b>✦ Letronna</b></footer>
</body></html>
"#
    )
}

impl Chat {
    /// Ask where, then write the session at `index` there as one web page.
    pub(crate) fn save_session_page(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fill_now(index);
        let c = &self.store.conversations[index];
        let html = page(&c.title, &c.messages);
        let name = super::sessions::export_name(&c.title);
        let name = if name.is_empty() {
            "Session".to_owned()
        } else {
            name
        };
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let receiver = cx.prompt_for_new_path(&home, Some(&format!("{name}.html")));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = receiver.await {
                let written = std::fs::write(&path, html).is_ok();
                let _ = this.update(cx, |this, cx| {
                    if written {
                        cx.reveal_path(&path);
                        this.toast(
                            "Saved: One page, nothing else needed. Open it in any browser or send it on.",
                            crate::ui::toast::Kind::Info,
                            cx,
                        );
                    } else {
                        this.error = Some("Could not write the page.".into());
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::page;
    use letronna_core::chat::Message;

    fn m(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_page_keeps_the_talk_counts_the_steps_and_escapes_html() {
        let html = page(
            "Fix <b>it</b>",
            &[
                m("user", "Fix the **total**"),
                m("tool", "secret tool output"),
                m("tool", "more"),
                m("assistant", "Done. <script>x</script>"),
            ],
        );
        assert!(html.contains("<title>Fix &lt;b&gt;it&lt;/b&gt;</title>"));
        assert!(html.contains("<strong>total</strong>"));
        assert!(html.contains("2 steps"));
        assert!(!html.contains("secret tool output"));
        assert!(!html.contains("<script>"));
    }
}
