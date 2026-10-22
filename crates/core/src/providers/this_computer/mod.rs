//! Models downloaded to this computer, run by llama.cpp's server, which
//! Letronna starts as a chat needs it (see `on_device`). Nothing to sign in
//! to: its models are what is downloaded, so listing them asks no server
//! and starts none.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct ThisComputer;

impl Provider for ThisComputer {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: crate::on_device::NAME,
            base: crate::on_device::BASE,
            blurb: "Models downloaded to this computer. Work offline; nothing leaves it.",
            docs: "https://letronna.ai",
            tier: Tier::Free,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::None
    }

    fn models(&self, _: &str, _: &str) -> Result<Vec<String>, String> {
        let ids = crate::on_device::shelf::ready_ids();
        crate::on_device::remember_all(&ids);
        Ok(ids.into_iter().map(str::to_owned).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_list_is_what_is_downloaded_and_no_server_is_asked() {
        let (_lock, _scratch) = crate::on_device::reach::tests::shelved("this-computer-list", &[]);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        assert_eq!(ThisComputer.models(&base, "k"), Ok(Vec::new()));
        drop(_scratch);
        drop(_lock);
        let (_lock, _scratch) =
            crate::on_device::reach::tests::shelved("this-computer-list-one", &["bonsai-2-4b"]);
        assert_eq!(
            ThisComputer.models(&base, "k"),
            Ok(vec!["bonsai-2-4b".to_owned()])
        );
        assert!(listener.accept().is_err(), "the list asked a server");
        let facts = crate::models::facts(crate::on_device::BASE, "bonsai-2-4b");
        assert!(
            facts
                .context
                .is_some_and(|c| c >= crate::on_device::LEAST_CONTEXT)
        );
        assert_eq!(facts.inputs, Some(vec!["text".to_owned()]));
    }
}
