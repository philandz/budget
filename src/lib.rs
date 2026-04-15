pub mod pb {
    pub mod service {
        pub mod budget {
            tonic::include_proto!("service.budget");
        }
    }
    pub mod common {
        pub mod base {
            tonic::include_proto!("common.base");
        }
    }
}

pub mod converters;
pub mod handler;
pub mod manager;
