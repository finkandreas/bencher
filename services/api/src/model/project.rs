use std::{str::FromStr, string::ToString};

use bencher_json::{JsonNewProject, JsonProject, ResourceId};
use diesel::{Insertable, QueryDsl, Queryable, RunQueryDsl, SqliteConnection};
use dropshot::{HttpError, RequestContext};
use url::Url;
use uuid::Uuid;

use super::{organization::QueryOrganization, user::QueryUser};
use crate::{
    diesel::ExpressionMethods,
    schema::{self, project as project_table},
    util::{map_http_error, resource_id::fn_resource_id, slug::unwrap_slug, Context},
};

#[derive(Insertable)]
#[diesel(table_name = project_table)]
pub struct InsertProject {
    pub uuid: String,
    pub organization_id: i32,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub url: Option<String>,
    pub public: bool,
}

impl InsertProject {
    pub fn from_json(
        conn: &mut SqliteConnection,
        project: JsonNewProject,
    ) -> Result<Self, HttpError> {
        let JsonNewProject {
            organization,
            name,
            slug,
            description,
            url,
            public,
        } = project;
        let slug = unwrap_slug!(conn, &name, slug, project, QueryProject);
        Ok(Self {
            uuid: Uuid::new_v4().to_string(),
            organization_id: QueryOrganization::from_resource_id(conn, &organization)?.id,
            name,
            slug,
            description,
            url: url.map(|u| u.to_string()),
            public,
        })
    }
}

fn_resource_id!(project);

#[derive(Queryable)]
pub struct QueryProject {
    pub id: i32,
    pub uuid: String,
    pub organization_id: i32,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub url: Option<String>,
    pub public: bool,
}

impl QueryProject {
    pub fn into_json(self, conn: &mut SqliteConnection) -> Result<JsonProject, HttpError> {
        let Self {
            id: _,
            uuid,
            organization_id,
            name,
            slug,
            description,
            url,
            public,
        } = self;
        Ok(JsonProject {
            uuid: Uuid::from_str(&uuid).map_err(map_http_error!("Failed to get project."))?,
            organization: QueryOrganization::get_uuid(conn, organization_id)?,
            name,
            slug,
            description,
            url: ok_url(url.as_deref())?,
            public,
        })
    }

    pub fn from_resource_id(
        conn: &mut SqliteConnection,
        project: &ResourceId,
    ) -> Result<Self, HttpError> {
        schema::project::table
            .filter(resource_id(project)?)
            .first::<QueryProject>(conn)
            .map_err(map_http_error!("Failed to get project."))
    }

    pub fn get_uuid(conn: &mut SqliteConnection, id: i32) -> Result<Uuid, HttpError> {
        let uuid: String = schema::project::table
            .filter(schema::project::id.eq(id))
            .select(schema::project::uuid)
            .first(conn)
            .map_err(map_http_error!("Failed to get project."))?;
        Uuid::from_str(&uuid).map_err(map_http_error!("Failed to get project."))
    }

    pub async fn connection(
        rqctx: &RequestContext<Context>,
        user_id: i32,
        project: &ResourceId,
    ) -> Result<i32, HttpError> {
        let context = &mut *rqctx.context().lock().await;
        let conn = &mut context.db_conn;

        let project = Self::from_resource_id(conn, project)?;
        QueryUser::has_access(conn, user_id, project.id)?;

        Ok(project.id)
    }
}

fn ok_url(url: Option<&str>) -> Result<Option<Url>, HttpError> {
    Ok(if let Some(url) = url {
        Some(Url::parse(url).map_err(map_http_error!("Failed to get project."))?)
    } else {
        None
    })
}
