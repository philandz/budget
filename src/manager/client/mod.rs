use tonic::transport::Channel;
use tonic::Status;

use crate::pb::service::identity::identity_service_client::IdentityServiceClient;
use crate::pb::service::identity::{GetOrgRoleRequest, GetProfileRequest, ListOrgMembersRequest};
use crate::pb::shared::organization::OrgRole;
use crate::pb::shared::user::UserType;

/// OrgMemberView enriched from identity service. We re-fetch per-budget
/// because the budget DB is on a different MySQL host than identity's
/// `users` table, so a JOIN across services is not possible.
#[derive(Debug, Clone)]
pub struct OrgUserInfo {
    pub user_id: String,
    pub display_name: String,
    pub email: String,
    pub avatar: String,
}

pub struct IdentityClient {
    inner: IdentityServiceClient<Channel>,
}

impl IdentityClient {
    pub async fn connect(url: &str) -> Result<Self, tonic::transport::Error> {
        let channel = Channel::from_shared(url.to_string())
            .expect("invalid identity gRPC URL")
            .connect()
            .await?;
        Ok(Self {
            inner: IdentityServiceClient::new(channel),
        })
    }

    pub async fn get_org_role(&mut self, user_id: &str, org_id: &str) -> Result<OrgRole, Status> {
        let resp = self
            .inner
            .get_org_role(tonic::Request::new(GetOrgRoleRequest {
                user_id: user_id.to_string(),
                org_id: org_id.to_string(),
            }))
            .await?;
        Ok(OrgRole::try_from(resp.into_inner().role).unwrap_or(OrgRole::OrNone))
    }

    pub async fn is_super_admin(&mut self, _user_id: &str) -> Result<bool, Status> {
        let resp = self
            .inner
            .get_profile(tonic::Request::new(GetProfileRequest {}))
            .await?;
        let user = resp.into_inner().user;
        let is_admin = user
            .map(|u| u.user_type == UserType::UtSuperAdmin as i32)
            .unwrap_or(false);
        Ok(is_admin)
    }

    pub fn is_super_admin_from_type(user_type: Option<&str>) -> bool {
        user_type == Some("super_admin")
    }

    /// Fetch every member of `org_id` from identity service. Used by
    /// budget and sharing to enrich `budget_members` / `participants`
    /// rows whose `display_name` and `email` columns are otherwise NULL
    /// because the budget DB sits on a different MySQL host than
    /// identity's `users` table — a cross-host JOIN is impossible.
    pub async fn list_org_users(
        &mut self,
        bearer: &str,
        org_id: &str,
    ) -> Result<Vec<OrgUserInfo>, Status> {
        let mut req = tonic::Request::new(ListOrgMembersRequest {
            org_id: org_id.to_string(),
        });
        // identity service's extract_bearer_token does its OWN
        // strip_prefix("Bearer "), so we must send the header with the
        // scheme prefix intact (i.e. forward the gateway's value verbatim).
        let value = tonic::metadata::MetadataValue::try_from(bearer)
            .map_err(|_| Status::unauthenticated("invalid bearer"))?;
        req.metadata_mut().insert("authorization", value);

        let resp = self.inner.list_org_members(req).await?.into_inner();
        Ok(resp
            .members
            .into_iter()
            .map(|m| OrgUserInfo {
                user_id: m.user_id,
                display_name: m.display_name,
                email: m.email,
                avatar: m.avatar,
            })
            .collect())
    }
}
