//! `attachment.put` / `attachment.import` / `attachment.remove`: a person's images and
//! files for a task (stored by [`crate::attachments`]), and the lines a message or a goal
//! carries for them. A person only (see [`crate::access`]).
//!
//! The registry is in memory: an attachment is known by its id until the daemon stops (an
//! unknown id is refused at send; the client uploads again). The files stay in the task's
//! session dir until the task is archived, which removes the whole dir.
use std::collections::HashMap;

use mirante_core::model::Lifecycle;
use mirante_ipc::types::{AttachmentImport, AttachmentKind, AttachmentPut, AttachmentRemove, AttachmentView};

use super::{Hub, HubError, Result};
use crate::attachments::{self as store, MAX_FILE, MAX_PER_MESSAGE};

/// One attachment the daemon knows.
#[derive(Debug, Clone)]
pub(crate) struct Known {
    view: AttachmentView,
    /// The stored file's name in the task's attachments folder.
    file: String,
    /// A message or a goal carried it: it is no longer removable.
    sent: bool,
}

/// Attachments by task id, then by id.
#[derive(Debug, Default)]
pub(crate) struct Registry(HashMap<String, Vec<Known>>);

/// What a message carries for its attachments: the text lines (one each) and the images'
/// absolute paths (Codex also gets them as `localImage` items).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Carried {
    pub(crate) lines: Vec<String>,
    pub(crate) images: Vec<String>,
}

impl Carried {
    /// `text` with the lines after it (after a blank line), or the lines alone.
    pub(crate) fn append_to(&self, text: &str) -> String {
        if self.lines.is_empty() {
            return text.to_string();
        }
        let lines = self.lines.join("\n");
        if text.trim().is_empty() {
            lines
        } else {
            format!("{}\n\n{lines}", text.trim_end())
        }
    }
}

/// The line a message carries for `v`.
pub(crate) fn line(v: &AttachmentView) -> String {
    match v.kind {
        AttachmentKind::Image => format!("[Attached image \"{}\": {}]", v.name, v.path),
        AttachmentKind::File if v.redacted > 0 => format!(
            "[Attached file \"{}\" ({}, {} secret{} removed): {}]",
            v.name,
            store::size(v.bytes),
            v.redacted,
            if v.redacted == 1 { "" } else { "s" },
            v.path
        ),
        AttachmentKind::File => format!("[Attached file \"{}\" ({}): {}]", v.name, store::size(v.bytes), v.path),
    }
}

impl Hub {
    /// The task's id, refused while it is archived or being archived.
    fn attachable(&self, task: &str) -> Result<String> {
        let t = self.lock().resolve(task)?;
        if matches!(t.lifecycle, Lifecycle::Archived | Lifecycle::Archiving) {
            return Err(HubError::Conflict(format!("task {} is archived", t.branch)));
        }
        Ok(t.id)
    }

    fn keep(&self, task_id: &str, name: &str, raw: Vec<u8>) -> Result<AttachmentView> {
        let name = store::sanitize(name);
        let p = store::prepare(&name, raw).map_err(HubError::Invalid)?;
        let mut reg = self.attachments.lock().unwrap_or_else(|p| p.into_inner());
        let s = store::store(self.paths.home(), task_id, &name, &p).map_err(HubError::Invalid)?;
        let view = AttachmentView {
            id: mirante_core::model::new_id(),
            path: s.path.to_string_lossy().into_owned(),
            name,
            kind: p.kind,
            bytes: s.bytes,
            redacted: p.redacted,
        };
        let known = Known { view: view.clone(), file: s.file, sent: false };
        reg.0.entry(task_id.to_string()).or_default().push(known);
        Ok(view)
    }

    /// `attachment.put`: bytes from the person's clipboard.
    pub fn attachment_put(&self, p: AttachmentPut) -> Result<AttachmentView> {
        let task_id = self.attachable(&p.task)?;
        let raw = store::decode_b64(&p.b64, MAX_FILE).map_err(HubError::Invalid)?;
        self.keep(&task_id, &p.name, raw)
    }

    /// `attachment.import`: a file the person dropped or picked, copied.
    pub fn attachment_import(&self, p: AttachmentImport) -> Result<AttachmentView> {
        let task_id = self.attachable(&p.task)?;
        let (name, raw) = store::read_source(&p.path, self.paths.home(), MAX_FILE).map_err(HubError::Invalid)?;
        self.keep(&task_id, &name, raw)
    }

    /// `attachment.remove`: deletes one not sent yet.
    pub fn attachment_remove(&self, p: AttachmentRemove) -> Result<()> {
        let task_id = self.lock().resolve(&p.task)?.id;
        let mut reg = self.attachments.lock().unwrap_or_else(|p| p.into_inner());
        let list = reg.0.get_mut(&task_id);
        let Some((at, k)) = list.as_ref().and_then(|l| l.iter().enumerate().find(|(_, k)| k.view.id == p.id)) else {
            return Err(HubError::NotFound(format!("no attachment {} on this task", p.id)));
        };
        if k.sent {
            return Err(HubError::Conflict(format!("attachment {} was sent: the agent may read it", k.view.name)));
        }
        let file = k.file.clone();
        store::unlink(self.paths.home(), &task_id, &file).map_err(HubError::Invalid)?;
        if let Some(l) = list {
            l.remove(at);
        }
        Ok(())
    }

    /// The lines (and image paths) for attachments `ids` of task `task_id`. Refused: more
    /// than [`MAX_PER_MESSAGE`], an id twice, or an id this task does not have (another
    /// task's, a removed one's, one from before a restart). Nothing is marked sent: the
    /// caller does that once the message was taken ([`Hub::mark_sent`]).
    pub(crate) fn carry_attachments(&self, task_id: &str, ids: &[String]) -> Result<Carried> {
        if ids.is_empty() {
            return Ok(Carried::default());
        }
        if ids.len() > MAX_PER_MESSAGE {
            return Err(HubError::Invalid(format!(
                "{} attachments; a message carries at most {MAX_PER_MESSAGE}",
                ids.len()
            )));
        }
        let reg = self.attachments.lock().unwrap_or_else(|p| p.into_inner());
        let list = reg.0.get(task_id).map_or(&[][..], Vec::as_slice);
        let mut at = Vec::with_capacity(ids.len());
        for id in ids {
            let i = list
                .iter()
                .position(|k| &k.view.id == id)
                .ok_or_else(|| HubError::Invalid(format!("unknown attachment {id}: attach the file again")))?;
            if at.contains(&i) {
                return Err(HubError::Invalid(format!("attachment {id} is listed twice")));
            }
            at.push(i);
        }
        let mut c = Carried::default();
        for i in at {
            let k = &list[i];
            c.lines.push(line(&k.view));
            if k.view.kind == AttachmentKind::Image {
                c.images.push(k.view.path.clone());
            }
        }
        Ok(c)
    }

    /// Attachments `ids` of task `task_id` were handed to a message or a goal: no longer
    /// removable.
    pub(crate) fn mark_sent(&self, task_id: &str, ids: &[String]) {
        let mut reg = self.attachments.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(list) = reg.0.get_mut(task_id) {
            list.iter_mut().filter(|k| ids.contains(&k.view.id)).for_each(|k| k.sent = true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testkit::Kit;
    use crate::server::dispatch;
    use mirante_core::model::AgentKind;
    use serde_json::json;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn b64(bytes: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn put_stores_an_image_and_a_redacted_text_and_returns_their_views() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "feat/att").task;
        let v: AttachmentView = serde_json::from_value(
            dispatch(&k.hub, "attachment.put", json!({ "task": t.id, "name": "../Screen Shot.png", "b64": b64(PNG) }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!((v.name.as_str(), v.kind, v.bytes, v.redacted), ("Screen_Shot.png", AttachmentKind::Image, 16, 0));
        assert!(v.path.ends_with(&format!("/sessions/{}/attachments/1-Screen_Shot.png", t.id)), "{}", v.path);
        assert_eq!(std::fs::read(&v.path).unwrap(), PNG);
        let token = format!("{}{}_{}", "gh", "p", "M1n2B3v4C5x6Z7l8K9j0H1g2F3d4S5a6P7o8");
        let text = format!("curl -H 'Authorization: Bearer {token}' https://x.test\n");
        let v = k
            .hub
            .attachment_put(AttachmentPut {
                task: t.id.clone(),
                name: "pasted-text.txt".into(),
                b64: b64(text.as_bytes()),
            })
            .unwrap();
        assert_eq!(v.kind, AttachmentKind::File);
        assert!(v.redacted >= 1);
        assert!(!std::fs::read_to_string(&v.path).unwrap().contains(&token));
        assert!(v.path.ends_with("/2-pasted-text.txt"), "{}", v.path);
        // Bad base64, an unknown task.
        let e = k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "a".into(), b64: "@@".into() });
        assert!(matches!(e, Err(HubError::Invalid(_))));
        let e = k.hub.attachment_put(AttachmentPut { task: "nope".into(), name: "a".into(), b64: b64(b"x") });
        assert!(matches!(e, Err(HubError::NotFound(_))), "{e:?}");
    }

    #[test]
    fn import_copies_the_file_and_refuses_mirantes_home() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "feat/imp").task;
        let src = k.dir.path().join("trace.har");
        let token = format!("{}{}_{}", "gh", "p", "Y1u2I3o4P5a6S7d8F9g0H1j2K3l4Z5x6C7v8");
        let har = json!({ "log": { "entries": [{ "request": { "url": "https://x.test/",
            "headers": [{ "name": "Cookie", "value": format!("s={token}") }] } }] } });
        std::fs::write(&src, har.to_string()).unwrap();
        let v = k
            .hub
            .attachment_import(AttachmentImport { task: t.id.clone(), path: src.to_string_lossy().into() })
            .unwrap();
        assert_eq!((v.name.as_str(), v.kind), ("trace.har", AttachmentKind::File));
        assert!(v.redacted >= 1);
        let stored = std::fs::read_to_string(&v.path).unwrap();
        assert!(!stored.contains(&token) && stored.contains("[redacted by Mirante]"), "{stored}");
        // The original is untouched.
        assert!(std::fs::read_to_string(&src).unwrap().contains(&token));
        let db = k.hub.paths().home().join("settings.json");
        std::fs::write(&db, "{}").unwrap();
        let e = k.hub.attachment_import(AttachmentImport { task: t.id.clone(), path: db.to_string_lossy().into() });
        assert!(matches!(&e, Err(HubError::Invalid(m)) if m.contains("Mirante's own folder")), "{e:?}");
        // Another task's stored attachment is inside the home too.
        let e = k.hub.attachment_import(AttachmentImport { task: t.id.clone(), path: v.path.clone() });
        assert!(matches!(&e, Err(HubError::Invalid(m)) if m.contains("Mirante's own folder")), "{e:?}");
    }

    #[test]
    fn remove_deletes_an_unsent_one_and_refuses_a_sent_or_unknown_one() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "feat/rm").task;
        let put = |name: &str| {
            k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: name.into(), b64: b64(b"hello") }).unwrap()
        };
        let (a, b) = (put("a.txt"), put("b.txt"));
        dispatch(&k.hub, "attachment.remove", json!({ "task": t.id, "id": a.id })).unwrap();
        assert!(!std::path::Path::new(&a.path).exists());
        let e = k.hub.attachment_remove(AttachmentRemove { task: t.id.clone(), id: a.id.clone() });
        assert!(matches!(e, Err(HubError::NotFound(_))));
        let c = k.hub.carry_attachments(&t.id, std::slice::from_ref(&b.id)).unwrap();
        assert_eq!(c.lines, [format!("[Attached file \"b.txt\" (5 B): {}]", b.path)]);
        // Carried lines alone do not make it sent; a taken message does.
        k.hub.mark_sent(&t.id, std::slice::from_ref(&b.id));
        let e = k.hub.attachment_remove(AttachmentRemove { task: t.id.clone(), id: b.id.clone() });
        assert!(matches!(e, Err(HubError::Conflict(_))));
        assert!(std::path::Path::new(&b.path).exists());
    }

    #[test]
    fn carried_lines_name_images_files_and_secrets_and_refuse_bad_ids() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "feat/carry").task;
        let other = k.create(AgentKind::Codex, "feat/other").task;
        let img =
            k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "s.png".into(), b64: b64(PNG) }).unwrap();
        let token = format!("{}{}_{}", "gh", "p", "T1y2U3i4O5p6A7s8D9f0G1h2J3k4L5z6X7c8");
        let log = k
            .hub
            .attachment_put(AttachmentPut {
                task: t.id.clone(),
                name: "a.log".into(),
                b64: b64(format!("{token}\n").as_bytes()),
            })
            .unwrap();
        let theirs = k
            .hub
            .attachment_put(AttachmentPut { task: other.id.clone(), name: "o.txt".into(), b64: b64(b"o") })
            .unwrap();
        let c = k.hub.carry_attachments(&t.id, &[img.id.clone(), log.id.clone()]).unwrap();
        assert_eq!(c.images, std::slice::from_ref(&img.path));
        assert_eq!(c.lines[0], format!("[Attached image \"s.png\": {}]", img.path));
        assert!(c.lines[1].starts_with("[Attached file \"a.log\" ("), "{}", c.lines[1]);
        assert!(c.lines[1].contains(", 1 secret removed): "), "{}", c.lines[1]);
        assert_eq!(c.append_to("look"), format!("look\n\n{}\n{}", c.lines[0], c.lines[1]));
        assert_eq!(c.append_to("  "), format!("{}\n{}", c.lines[0], c.lines[1]));
        // Another task's id, an unknown one, one twice, too many.
        assert!(k.hub.carry_attachments(&t.id, &[theirs.id]).is_err());
        assert!(k.hub.carry_attachments(&t.id, &["nope".into()]).is_err());
        assert!(k.hub.carry_attachments(&t.id, &[img.id.clone(), img.id.clone()]).is_err());
        let many = vec![img.id.clone(); MAX_PER_MESSAGE + 1];
        assert!(k.hub.carry_attachments(&t.id, &many).unwrap_err().to_string().contains("at most 20"));
    }

    fn wire(k: &Kit) -> Vec<String> {
        k.factory.driver.wire.lock().unwrap().clone()
    }

    fn send(k: &Kit, task: &str, text: &str, ids: Vec<String>) -> Result<mirante_ipc::types::SendResult> {
        let mode = mirante_ipc::types::SendMode::Steer;
        k.hub.send(mirante_ipc::types::SendMessage { task: task.into(), text: text.into(), mode, attachments: ids })
    }

    #[test]
    fn a_message_carries_its_attachments_lines_after_the_person_s_words() {
        use mirante_core::delivery::label_operator;
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "feat/msg").task;
        let img =
            k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "s.png".into(), b64: b64(PNG) }).unwrap();
        let txt =
            k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "n.txt".into(), b64: b64(b"hi") }).unwrap();
        send(&k, &t.id, "what is this?", vec![img.id.clone(), txt.id.clone()]).unwrap();
        let want = format!("what is this?\n\n{}\n{}", line(&img), line(&txt));
        assert_eq!(wire(&k), [label_operator(&want)]);
        // Claude's typist takes the same text (one bracketed paste).
        assert!(crate::driver::paste::typed_message(&label_operator(&want)).is_ok());
        // Sent: no longer removable.
        let e = k.hub.attachment_remove(AttachmentRemove { task: t.id.clone(), id: img.id.clone() });
        assert!(matches!(e, Err(HubError::Conflict(_))), "{e:?}");
        // Attachments alone: no text needed. Through the socket's dispatch, too.
        let more =
            k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "m.txt".into(), b64: b64(b"m") }).unwrap();
        dispatch(
            &k.hub,
            "message.send",
            json!({ "task": t.id, "text": "", "mode": "steer", "attachments": [more.id] }),
        )
        .unwrap();
        assert_eq!(wire(&k)[1], label_operator(&line(&more)));
    }

    #[test]
    fn a_message_with_bad_attachments_is_refused_whole_and_the_size_check_counts_words_only() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "feat/bad").task;
        let a =
            k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "a.txt".into(), b64: b64(b"a") }).unwrap();
        assert!(matches!(send(&k, &t.id, "x", vec!["nope".into()]), Err(HubError::Invalid(_))));
        let big = "y".repeat(crate::hub::messages::MAX_MESSAGE_BYTES + 1);
        assert!(matches!(send(&k, &t.id, &big, vec![a.id.clone()]), Err(HubError::Invalid(_))));
        assert!(wire(&k).is_empty());
        // Refused: still removable.
        let full = "y".repeat(crate::hub::messages::MAX_MESSAGE_BYTES);
        send(&k, &t.id, &full, vec![a.id.clone()]).unwrap();
        assert!(wire(&k)[0].len() > crate::hub::messages::MAX_MESSAGE_BYTES);
        // Without attachments an empty text is still refused; an agent never attaches.
        assert!(matches!(send(&k, &t.id, " ", vec![]), Err(HubError::Invalid(_))));
        let m = mirante_ipc::types::SendMessage {
            task: t.id.clone(),
            text: "x".into(),
            mode: mirante_ipc::types::SendMode::Steer,
            attachments: vec![a.id.clone()],
        };
        assert!(matches!(k.hub.send_by(m, "agent"), Err(HubError::Invalid(_))));
    }

    #[test]
    fn a_goal_carries_its_attachments_and_may_be_attachments_only() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "feat/goal").task;
        let img =
            k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "s.png".into(), b64: b64(PNG) }).unwrap();
        let start =
            mirante_ipc::types::StartSession { task: t.id.clone(), prompt: None, attachments: vec!["nope".into()] };
        assert!(matches!(k.hub.start_session(start, false), Err(HubError::Invalid(_))));
        dispatch(&k.hub, "session.start", json!({ "task": t.id, "prompt": "", "attachments": [img.id] })).unwrap();
        k.emit(crate::driver::DriverEvent::Boot(mirante_ipc::types::BootState::Ready));
        assert_eq!(wire(&k), [line(&img)]);
        let e = k.hub.attachment_remove(AttachmentRemove { task: t.id.clone(), id: img.id.clone() });
        assert!(matches!(e, Err(HubError::Conflict(_))), "{e:?}");
    }

    #[test]
    fn an_archived_task_takes_no_attachments() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "feat/arch").task;
        k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "a.txt".into(), b64: b64(b"x") }).unwrap();
        let dir = k.hub.paths().session_dir(&t.id).join("attachments");
        assert!(dir.exists());
        k.hub.archive_with(mirante_ipc::types::ArchiveTask { task: t.id.clone(), ..Default::default() }).unwrap();
        // Archiving removed the session dir, attachments included.
        assert!(!dir.exists());
        let e = k.hub.attachment_put(AttachmentPut { task: t.id.clone(), name: "a.txt".into(), b64: b64(b"x") });
        assert!(matches!(e, Err(HubError::Conflict(_))), "{e:?}");
    }
}
