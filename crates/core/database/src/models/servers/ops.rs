use revolt_result::Result;

use revolt_permissions::OverrideField;

use crate::{FieldsRole, FieldsServer, PartialRole, PartialServer, Role, Server};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractServers: Sync + Send {
    /// Insert a new server into database
    async fn insert_server(&self, server: &Server) -> Result<()>;

    /// Fetch a server by its id
    async fn fetch_server(&self, id: &str) -> Result<Server>;

    /// Fetch a servers by their ids
    async fn fetch_servers<'a>(&self, ids: &'a [String]) -> Result<Vec<Server>>;

    /// Update a server with new information
    async fn update_server(
        &self,
        id: &str,
        partial: &PartialServer,
        remove: Vec<FieldsServer>,
    ) -> Result<()>;

    /// Set what one category gives a role (or, with no role, everyone)
    ///
    /// Changes only that one entry, so it cannot undo a concurrent edit to
    /// another category or role the way rewriting the whole list would.
    async fn set_category_permission(
        &self,
        server_id: &str,
        category_id: &str,
        role_id: Option<&str>,
        permissions: OverrideField,
    ) -> Result<()>;

    /// Delete a server by its id
    async fn delete_server(&self, id: &str) -> Result<()>;

    /// Insert a new role into server object
    async fn insert_role(&self, server_id: &str, role: &Role) -> Result<()>;

    /// Update an existing role on a server
    async fn update_role(
        &self,
        server_id: &str,
        role_id: &str,
        partial: &PartialRole,
        remove: Vec<FieldsRole>,
    ) -> Result<()>;

    /// Delete a role from a server
    ///
    /// Also updates channels and members.
    async fn delete_role(&self, server_id: &str, role_id: &str) -> Result<()>;
}
