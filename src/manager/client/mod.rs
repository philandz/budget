use tonic::transport::Channel;
use tonic::Status;

use crate::pb::service::identity::identity_service_client::IdentityServiceClient;
use crate::pb::service::identity::{GetOrgRoleRequest, GetProfileRequest};
use crate::pb::shared::organization::OrgRole;
use crate::pb::shared::user::UserType;

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
}
