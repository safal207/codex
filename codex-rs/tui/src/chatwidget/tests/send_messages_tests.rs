use super::*;
use codex_send::SendRequest;
use codex_send::SendResponse;
use pretty_assertions::assert_eq;

fn request(id: &str, text: &str) -> SendRequest {
    SendRequest {
        id: id.to_string(),
        text: text.to_string(),
    }
}

fn message_scope(chat: &ChatWidget) -> crate::app_event::SendMessageScope {
    crate::app_event::SendMessageScope {
        thread_id: chat.thread_id.unwrap_or_else(ThreadId::new),
        endpoint_generation: uuid::Uuid::new_v4(),
    }
}

fn plain_text_turn(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

#[tokio::test]
async fn sent_message_submits_a_turn_when_the_session_is_idle() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    let text = "Ghidra is available.\n\nYou can acquire the lock.";

    let scope = message_scope(&chat);
    let response = chat.submit_sent_message(scope, request("r1", text));

    assert_eq!(response, SendResponse::Submitted);
    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected a user turn");
    };
    assert_eq!(items, plain_text_turn(text));
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::FollowIpcTranscript { scope: delivered_scope } if delivered_scope == scope))
    );
}

#[tokio::test]
async fn sent_message_steers_a_running_turn_instead_of_waiting() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();

    let scope = message_scope(&chat);
    let response = chat.submit_sent_message(scope, request("r1", "status?"));

    assert_eq!(response, SendResponse::Submitted);
    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected a user turn");
    };
    assert_eq!(items, plain_text_turn("status?"));
    assert!(chat.input_queue.queued_user_messages.is_empty());
}

#[tokio::test]
async fn sent_message_is_queued_until_the_session_is_configured() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let scope = message_scope(&chat);
    let response = chat.submit_sent_message(scope, request("r1", "hello"));

    assert_eq!(response, SendResponse::Queued);
    assert_eq!(
        chat.input_queue
            .queued_user_messages
            .iter()
            .map(|queued| queued.user_message.text.clone())
            .collect::<Vec<_>>(),
        vec!["hello".to_string()]
    );
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn repeated_request_id_is_acknowledged_without_a_second_submission() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());

    let scope = message_scope(&chat);
    let first = chat.submit_sent_message(scope, request("r1", "once"));
    let second = chat.submit_sent_message(scope, request("r1", "once"));

    assert_eq!(
        (first, second),
        (SendResponse::Submitted, SendResponse::Submitted)
    );
    assert_matches!(next_submit_op(&mut op_rx), Op::UserTurn { .. });
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn sent_message_delivers_shell_escapes_and_slash_commands_as_plain_text() {
    for text in ["!rm -rf scratch", "/compact", "/quit now"] {
        let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.thread_id = Some(ThreadId::new());

        let scope = message_scope(&chat);
        let response = chat.submit_sent_message(scope, request("r1", text));

        assert_eq!(response, SendResponse::Submitted, "{text}");
        let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
            panic!("expected `{text}` to be a plain user turn");
        };
        assert_eq!(items, plain_text_turn(text));
        assert!(op_rx.try_recv().is_err(), "{text}");
    }
}

#[tokio::test]
async fn queued_sent_message_stays_literal_when_the_queue_drains() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let scope = message_scope(&chat);
    let response = chat.submit_sent_message(scope, request("r1", "!rm -rf scratch"));

    assert_eq!(response, SendResponse::Queued);
    let queued = &chat.input_queue.queued_user_messages;
    assert_eq!(queued.len(), 1);
    assert!(matches!(queued[0].action, QueuedInputAction::Literal));

    chat.thread_id = Some(scope.thread_id);
    chat.maybe_send_next_queued_input();
    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected the drained message to be a plain user turn, not a shell command");
    };
    assert_eq!(items, plain_text_turn("!rm -rf scratch"));
}

#[tokio::test]
async fn queued_sent_message_is_not_submitted_to_a_different_widget_thread() {
    let (mut chat, _events, mut ops) = make_chatwidget_manual(None).await;
    let scope = message_scope(&chat);
    assert_eq!(
        chat.submit_sent_message(scope, request("queued", "marker")),
        SendResponse::Queued
    );
    chat.thread_id = Some(ThreadId::new());
    chat.maybe_send_next_queued_input();
    assert!(ops.try_recv().is_err());
}

#[tokio::test]
async fn rejected_steer_recovery_never_merges_ipc_scope_into_manual_input() {
    let (mut chat, _events, _ops) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(ThreadId::new());
    let first = message_scope(&chat);
    let second = message_scope(&chat);
    for (text, source) in [
        ("manual before", UserMessageSource::Prompt),
        ("!first IPC", UserMessageSource::Ipc(first)),
        ("second IPC", UserMessageSource::Ipc(second)),
        ("manual after", UserMessageSource::Prompt),
    ] {
        chat.input_queue
            .rejected_steers_queue
            .push_back(UserMessage::from(text));
        chat.input_queue.rejected_steer_sources.push_back(source);
        chat.input_queue
            .rejected_steer_history_records
            .push_back(UserMessageHistoryRecord::UserMessageText);
    }
    for (text, source) in [
        ("manual before", UserMessageSource::Prompt),
        ("!first IPC", UserMessageSource::Ipc(first)),
        ("second IPC", UserMessageSource::Ipc(second)),
        ("manual after", UserMessageSource::Prompt),
    ] {
        let (message, _) = chat
            .pop_next_queued_user_message()
            .expect("recovered steer");
        assert_eq!(message.user_message.text, text);
        assert_eq!(message.source, source);
        if source.ipc_scope().is_some() {
            assert!(matches!(message.action, QueuedInputAction::Literal));
        }
    }
    assert!(chat.pop_next_queued_user_message().is_none());
}

#[tokio::test]
async fn interrupted_ipc_steer_retry_keeps_scope_and_literal_text() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.codex_op_target = CodexOpTarget::AppEvent;
    chat.on_task_started();
    let scope = message_scope(&chat);
    assert_eq!(
        chat.submit_sent_message(scope, request("interrupted", "!literal marker")),
        SendResponse::Submitted
    );
    while events.try_recv().is_ok() {}
    chat.input_queue.submit_pending_steers_after_interrupt = true;
    chat.on_interrupted_turn(TurnAbortReason::Interrupted);
    let (retried_scope, op) = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::SubmitIpcThreadOp { scope, op } => Some((scope, op)),
            AppEvent::CodexOp(_) => panic!("IPC recovery must not emit an unscoped operation"),
            _ => None,
        })
        .expect("interrupted IPC steer should be retried with its scope");
    assert_eq!(retried_scope, scope);
    let Op::UserTurn { items, .. } = op else {
        panic!("IPC text must remain a user turn")
    };
    assert_eq!(items, plain_text_turn("!literal marker"));
    assert!(ops.try_recv().is_err());
}
