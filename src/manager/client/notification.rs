//! gRPC sink: delivers [`NotifyEvent`]s from the outbox to `NotificationService.Publish`.

use async_trait::async_trait;
use philand_event::{EventSink, NotifyCategory, NotifyEvent, NotifySeverity, SinkError};
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::{Code, Request};

use crate::pb::service::notification as pb;
use crate::pb::service::notification::notification_service_client::NotificationServiceClient;

pub struct GrpcNotificationSink {
    client: NotificationServiceClient<Channel>,
    token: MetadataValue<tonic::metadata::Ascii>,
}

impl GrpcNotificationSink {
    /// Lazy channel: the notification service may start after this one.
    pub fn new(url: &str, internal_token: &str) -> anyhow::Result<Self> {
        let channel = Channel::from_shared(url.to_string())
            .map_err(|e| anyhow::anyhow!("invalid NOTIFICATION_GRPC_URL: {e}"))?
            .connect_lazy();
        let token = MetadataValue::try_from(internal_token)
            .map_err(|_| anyhow::anyhow!("INTERNAL_SERVICE_TOKEN is not a valid header value"))?;
        Ok(Self {
            client: NotificationServiceClient::new(channel),
            token,
        })
    }
}

pub fn category_to_pb(c: NotifyCategory) -> i32 {
    (match c {
        NotifyCategory::Activity => pb::NotificationCategory::Activity,
        NotifyCategory::Membership => pb::NotificationCategory::Membership,
        NotifyCategory::Reminder => pb::NotificationCategory::Reminder,
        NotifyCategory::Alert => pb::NotificationCategory::Alert,
        NotifyCategory::Security => pb::NotificationCategory::Security,
        NotifyCategory::System => pb::NotificationCategory::System,
    }) as i32
}

pub fn severity_to_pb(s: NotifySeverity) -> i32 {
    (match s {
        NotifySeverity::Info => pb::NotificationSeverity::Info,
        NotifySeverity::Warning => pb::NotificationSeverity::Warning,
        NotifySeverity::Critical => pb::NotificationSeverity::Critical,
    }) as i32
}

pub fn to_publish_request(e: &NotifyEvent) -> pb::PublishRequest {
    pb::PublishRequest {
        event_id: e.event_id.clone(),
        source: e.source.clone(),
        r#type: e.event_type.clone(),
        category: category_to_pb(e.category),
        severity: severity_to_pb(e.severity),
        recipient_user_ids: e.recipient_user_ids.clone(),
        actor_user_id: e.actor_user_id.clone().unwrap_or_default(),
        org_id: e.org_id.clone().unwrap_or_default(),
        budget_id: e.budget_id.clone().unwrap_or_default(),
        params_json: e.params.to_string(),
        link: e.link.clone().unwrap_or_default(),
        fallback_text: e.fallback_text.clone(),
        dedup_key: e.dedup_key.clone().unwrap_or_default(),
        group_key: e.group_key.clone().unwrap_or_default(),
        occurred_at: e.occurred_at,
    }
}

/// `InvalidArgument` can never succeed on retry; everything else (including auth errors,
/// which are a deployment problem an operator can fix) is retried with backoff.
pub fn classify(code: Code, message: &str) -> SinkError {
    match code {
        Code::InvalidArgument => SinkError::Permanent(message.to_string()),
        _ => SinkError::Retryable(format!("{code:?}: {message}")),
    }
}

#[async_trait]
impl EventSink for GrpcNotificationSink {
    async fn publish(&self, event: &NotifyEvent) -> Result<(), SinkError> {
        let mut req = Request::new(to_publish_request(event));
        req.metadata_mut()
            .insert("x-internal-token", self.token.clone());
        req.set_timeout(std::time::Duration::from_secs(10));
        let mut client = self.client.clone();
        client
            .publish(req)
            .await
            .map(|_| ())
            .map_err(|s| classify(s.code(), s.message()))
    }
}
