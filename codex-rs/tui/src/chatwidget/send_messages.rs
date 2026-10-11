//! Delivery of `codex send` messages into the chat surface. Sent text follows
//! the composer's Enter path and is delivered as plain text: `!` and `/`
//! prefixes are not interpreted.

use super::*;
use crate::chatwidget::input_flow::UserMessageDisposition;
use codex_send::SendRequest;
use codex_send::SendResponse;

const REMEMBERED_SEND_REQUEST_IDS: usize = 64;

impl ChatWidget {
    pub(crate) fn submit_sent_message(
        &mut self,
        scope: crate::app_event::SendMessageScope,
        request: SendRequest,
    ) -> SendResponse {
        if self
            .thread_id
            .is_some_and(|thread_id| thread_id != scope.thread_id)
        {
            return SendResponse::Rejected {
                message: "the IPC recipient is not the displayed thread".to_string(),
            };
        }
        if let Some((_, response)) = self
            .recent_send_responses
            .iter()
            .find(|(id, _)| *id == request.id)
        {
            return response.clone();
        }
        let SendRequest { id, text } = request;
        let user_message = UserMessage {
            text,
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        };
        let response = match self.submit_or_queue_user_message(
            user_message,
            ShellEscapePolicy::Disallow,
            UserMessageSource::Ipc(scope),
        ) {
            UserMessageDisposition::Submitted => SendResponse::Submitted,
            UserMessageDisposition::Queued => SendResponse::Queued,
            UserMessageDisposition::Dropped => SendResponse::Rejected {
                message: "the session is not accepting input".to_string(),
            },
        };
        if self.recent_send_responses.len() == REMEMBERED_SEND_REQUEST_IDS {
            self.recent_send_responses.pop_front();
        }
        self.recent_send_responses.push_back((id, response.clone()));
        response
    }
}
