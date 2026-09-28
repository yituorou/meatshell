use super::*;

pub(super) fn blank_forward_draft() -> PortFwd {
    PortFwd {
        id: "".into(),
        kind: "local".into(),
        auto_start: true,
        name: "".into(),
        bind_addr: "127.0.0.1".into(),
        bind_port: "".into(),
        host: "".into(),
        host_port: "".into(),
    }
}

pub(super) fn forward_drafts(forwards: &[crate::config::PortForward]) -> Vec<PortFwd> {
    forwards
        .iter()
        .map(|forward| PortFwd {
            id: forward.id.clone().into(),
            kind: forward.kind.clone().into(),
            auto_start: forward.auto_start,
            name: forward.name.clone().into(),
            bind_addr: if forward.bind_addr.trim().is_empty() {
                "127.0.0.1".into()
            } else {
                forward.bind_addr.trim().into()
            },
            bind_port: forward.bind_port.to_string().into(),
            host: forward.host.clone().into(),
            host_port: if forward.kind == "dynamic" {
                "".into()
            } else {
                forward.host_port.to_string().into()
            },
        })
        .collect()
}

pub(super) fn forward_model(forwards: &[PortFwd]) -> ModelRc<PortFwd> {
    ModelRc::from(Rc::new(VecModel::from(forwards.to_vec())))
}

pub(super) fn validated_port_forwards(
    drafts: &[PortFwd],
) -> std::result::Result<Vec<crate::config::PortForward>, String> {
    let mut forwards = Vec::new();
    for draft in drafts {
        let is_blank = draft.name.trim().is_empty()
            && draft.bind_port.trim().is_empty()
            && draft.host.trim().is_empty()
            && draft.host_port.trim().is_empty();
        if is_blank {
            continue;
        }

        let bind_port = draft
            .bind_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| {
                t(
                    "请输入有效的监听端口（1-65535）",
                    "Enter a valid listen port (1-65535).",
                )
                .to_string()
            })?;
        let kind = draft.kind.as_str();
        let (host, host_port) = if kind == "dynamic" {
            (String::new(), 0)
        } else {
            let host = draft.host.trim();
            let host_port = draft
                .host_port
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0);
            if host.is_empty() || host_port.is_none() {
                return Err(t(
                    "请输入目标主机和有效的目标端口（1-65535）",
                    "Enter a target host and a valid target port (1-65535).",
                )
                .to_string());
            }
            (host.to_string(), host_port.unwrap())
        };

        forwards.push(crate::config::PortForward {
            id: draft.id.to_string(),
            kind: kind.to_string(),
            auto_start: draft.auto_start,
            name: draft.name.trim().to_string(),
            bind_addr: if draft.bind_addr.trim().is_empty() {
                "127.0.0.1".to_string()
            } else {
                draft.bind_addr.trim().to_string()
            },
            bind_port,
            host,
            host_port,
        });
    }
    Ok(forwards)
}

#[cfg(test)]
#[path = "../../tests/app/port_forwarding/mod.rs"]
mod port_forward_draft_tests;
