use bencher_json::{
    organization::{member::OrganizationRole, JsonUpdateOrganization},
    DateTime, JsonDirection, JsonNewOrganization, JsonOrganization, JsonOrganizations,
    JsonPagination, ResourceId, ResourceName,
};
use bencher_rbac::organization::Permission;
use diesel::{
    BoolExpressionMethods, ExpressionMethods, QueryDsl, RunQueryDsl, TextExpressionMethods,
};
use dropshot::{endpoint, HttpError, Path, Query, RequestContext, TypedBody};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    conn_lock,
    context::ApiContext,
    endpoints::{
        endpoint::{CorsResponse, Get, Patch, Post, ResponseCreated, ResponseOk},
        Endpoint,
    },
    error::{resource_conflict_err, resource_not_found_err},
    model::{
        organization::{
            organization_role::InsertOrganizationRole, InsertOrganization, QueryOrganization,
            UpdateOrganization,
        },
        user::{
            admin::AdminUser,
            auth::{AuthUser, BearerToken},
        },
    },
    schema,
    util::{headers::TotalCount, search::Search},
};

pub type OrganizationsPagination = JsonPagination<OrganizationsSort>;

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationsSort {
    /// Sort by organization name.
    #[default]
    Name,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OrganizationsQuery {
    /// Filter by organization name, exact match.
    pub name: Option<ResourceName>,
    /// Search by organization name, slug, or UUID.
    pub search: Option<Search>,
}

#[allow(clippy::no_effect_underscore_binding, clippy::unused_async)]
#[endpoint {
    method = OPTIONS,
    path =  "/v0/organizations",
    tags = ["organizations"]
}]
pub async fn organizations_options(
    _rqctx: RequestContext<ApiContext>,
    _pagination_params: Query<OrganizationsPagination>,
    _query_params: Query<OrganizationsQuery>,
) -> Result<CorsResponse, HttpError> {
    Ok(Endpoint::cors(&[Get.into(), Post.into()]))
}

/// List organizations
///
/// List all organizations where the user is a member.
/// The user must have `view` permissions for each organization.
/// By default, the organizations are sorted in alphabetical order by name.
/// The HTTP response header `X-Total-Count` contains the total number of organizations.
#[endpoint {
    method = GET,
    path = "/v0/organizations",
    tags = ["organizations"]
}]
pub async fn organizations_get(
    rqctx: RequestContext<ApiContext>,
    bearer_token: BearerToken,
    pagination_params: Query<OrganizationsPagination>,
    query_params: Query<OrganizationsQuery>,
) -> Result<ResponseOk<JsonOrganizations>, HttpError> {
    let auth_user = AuthUser::from_token(rqctx.context(), bearer_token).await?;
    let (json, total_count) = get_ls_inner(
        rqctx.context(),
        &auth_user,
        pagination_params.into_inner(),
        query_params.into_inner(),
    )
    .await?;
    Ok(Get::auth_response_ok_with_total_count(json, total_count))
}

async fn get_ls_inner(
    context: &ApiContext,
    auth_user: &AuthUser,
    pagination_params: OrganizationsPagination,
    query_params: OrganizationsQuery,
) -> Result<(JsonOrganizations, TotalCount), HttpError> {
    let organizations = get_ls_query(context, auth_user, &pagination_params, &query_params)
        .offset(pagination_params.offset())
        .limit(pagination_params.limit())
        .load::<QueryOrganization>(conn_lock!(context))
        .map_err(resource_not_found_err!(
            Organization,
            (auth_user, &pagination_params, &query_params)
        ))?;

    // Drop connection lock before iterating
    let json_organizations = organizations
        .into_iter()
        .map(QueryOrganization::into_json)
        .collect();

    let total_count = get_ls_query(context, auth_user, &pagination_params, &query_params)
        .count()
        .get_result::<i64>(conn_lock!(context))
        .map_err(resource_not_found_err!(
            Organization,
            (auth_user, &pagination_params, &query_params)
        ))?
        .try_into()?;

    Ok((json_organizations, total_count))
}

fn get_ls_query<'q>(
    context: &ApiContext,
    auth_user: &AuthUser,
    pagination_params: &OrganizationsPagination,
    query_params: &'q OrganizationsQuery,
) -> schema::organization::BoxedQuery<'q, diesel::sqlite::Sqlite> {
    let mut query = schema::organization::table.into_boxed();

    if !auth_user.is_admin(&context.rbac) {
        let organizations = auth_user.organizations(&context.rbac, Permission::View);
        query = query.filter(schema::organization::id.eq_any(organizations));
    }

    if let Some(name) = query_params.name.as_ref() {
        query = query.filter(schema::organization::name.eq(name));
    }
    if let Some(search) = query_params.search.as_ref() {
        query = query.filter(
            schema::organization::name
                .like(search)
                .or(schema::organization::slug.like(search))
                .or(schema::organization::uuid.like(search)),
        );
    }

    match pagination_params.order() {
        OrganizationsSort::Name => match pagination_params.direction {
            Some(JsonDirection::Asc) | None => query.order((
                schema::organization::name.asc(),
                schema::organization::slug.asc(),
            )),
            Some(JsonDirection::Desc) => query.order((
                schema::organization::name.desc(),
                schema::organization::slug.desc(),
            )),
        },
    }
}

/// Create an organization
///
/// Create a new organization.
/// The user must be an admin on the server to use this route.
#[endpoint {
    method = POST,
    path = "/v0/organizations",
    tags = ["organizations"]
}]
pub async fn organization_post(
    rqctx: RequestContext<ApiContext>,
    bearer_token: BearerToken,
    body: TypedBody<JsonNewOrganization>,
) -> Result<ResponseCreated<JsonOrganization>, HttpError> {
    let admin_user = AdminUser::from_token(rqctx.context(), bearer_token).await?;
    let json = post_inner(rqctx.context(), body.into_inner(), &admin_user).await?;
    Ok(Post::auth_response_created(json))
}

async fn post_inner(
    context: &ApiContext,
    json_organization: JsonNewOrganization,
    admin_user: &AdminUser,
) -> Result<JsonOrganization, HttpError> {
    // Create the organization
    let insert_organization =
        InsertOrganization::from_json(conn_lock!(context), json_organization)?;
    diesel::insert_into(schema::organization::table)
        .values(&insert_organization)
        .execute(conn_lock!(context))
        .map_err(resource_conflict_err!(Organization, insert_organization))?;
    let query_organization = schema::organization::table
        .filter(schema::organization::uuid.eq(&insert_organization.uuid))
        .first::<QueryOrganization>(conn_lock!(context))
        .map_err(resource_not_found_err!(Organization, insert_organization))?;

    let timestamp = DateTime::now();
    // Connect the user to the organization as a `Maintainer`
    let insert_org_role = InsertOrganizationRole {
        user_id: admin_user.user().id,
        organization_id: query_organization.id,
        role: OrganizationRole::Leader,
        created: timestamp,
        modified: timestamp,
    };
    diesel::insert_into(schema::organization_role::table)
        .values(&insert_org_role)
        .execute(conn_lock!(context))
        .map_err(resource_conflict_err!(OrganizationRole, insert_org_role))?;

    Ok(query_organization.into_json())
}

#[derive(Deserialize, JsonSchema)]
pub struct OrganizationParams {
    /// The slug or UUID for an organization.
    pub organization: ResourceId,
}

#[allow(clippy::no_effect_underscore_binding, clippy::unused_async)]
#[endpoint {
    method = OPTIONS,
    path =  "/v0/organizations/{organization}",
    tags = ["organizations"]
}]
pub async fn organization_options(
    _rqctx: RequestContext<ApiContext>,
    _path_params: Path<OrganizationParams>,
) -> Result<CorsResponse, HttpError> {
    Ok(Endpoint::cors(&[Get.into(), Patch.into()]))
}

/// View an organization
///
/// View an organization where the user is a member.
/// The user must have `view` permissions for the organization.
#[endpoint {
    method = GET,
    path = "/v0/organizations/{organization}",
    tags = ["organizations"]
}]
pub async fn organization_get(
    rqctx: RequestContext<ApiContext>,
    bearer_token: BearerToken,
    path_params: Path<OrganizationParams>,
) -> Result<ResponseOk<JsonOrganization>, HttpError> {
    let auth_user = AuthUser::from_token(rqctx.context(), bearer_token).await?;
    let json = get_one_inner(rqctx.context(), path_params.into_inner(), &auth_user).await?;
    Ok(Get::auth_response_ok(json))
}

async fn get_one_inner(
    context: &ApiContext,
    path_params: OrganizationParams,
    auth_user: &AuthUser,
) -> Result<JsonOrganization, HttpError> {
    Ok(QueryOrganization::is_allowed_resource_id(
        conn_lock!(context),
        &context.rbac,
        &path_params.organization,
        auth_user,
        Permission::View,
    )?
    .into_json())
}

/// Update an organization
///
/// Update an organization where the user is a member.
/// The user must have `edit` permissions for the organization.
/// If updating the license, the user must have `manage` permissions for the organization.
#[endpoint {
    method = PATCH,
    path =  "/v0/organizations/{organization}",
    tags = ["organizations"]
}]
pub async fn organization_patch(
    rqctx: RequestContext<ApiContext>,
    bearer_token: BearerToken,
    path_params: Path<OrganizationParams>,
    body: TypedBody<JsonUpdateOrganization>,
) -> Result<ResponseOk<JsonOrganization>, HttpError> {
    let auth_user = AuthUser::from_token(rqctx.context(), bearer_token).await?;
    let json = patch_inner(
        rqctx.context(),
        path_params.into_inner(),
        body.into_inner(),
        &auth_user,
    )
    .await?;
    Ok(Patch::auth_response_ok(json))
}

async fn patch_inner(
    context: &ApiContext,
    path_params: OrganizationParams,
    json_organization: JsonUpdateOrganization,
    auth_user: &AuthUser,
) -> Result<JsonOrganization, HttpError> {
    #[cfg(not(feature = "plus"))]
    let permission = Permission::Edit;
    #[cfg(feature = "plus")]
    let license = json_organization.license();
    #[cfg(feature = "plus")]
    let permission = if license.is_some() {
        // Manage permission is required to update the license
        Permission::Manage
    } else {
        Permission::Edit
    };
    let query_organization = QueryOrganization::is_allowed_resource_id(
        conn_lock!(context),
        &context.rbac,
        &path_params.organization,
        auth_user,
        permission,
    )?;
    #[cfg(feature = "plus")]
    if let Some(license) = license {
        // If updating the license make sure that the server is not Bencher Cloud
        // All Bencher Cloud license updates should be handled via plans directly
        // Only Self-Hosted should be able to update the license
        if context.is_bencher_cloud {
            return Err(crate::error::locked_error(
                "Direct license updates are not allowed on Bencher Cloud. Please update your plan instead.",
            ));
        }
        // If updating a Self-Hosted license make sure that it is actually valid for this particular organization
        context
            .licensor
            .validate_organization(license, query_organization.uuid)
            .map_err(resource_not_found_err!(
                Organization,
                (license, &query_organization)
            ))?;
    }

    let organization_query =
        schema::organization::table.filter(schema::organization::id.eq(query_organization.id));
    let update_organization = UpdateOrganization::from(json_organization);
    diesel::update(organization_query)
        .set(&update_organization)
        .execute(conn_lock!(context))
        .map_err(resource_conflict_err!(Organization, update_organization))?;

    Ok(QueryOrganization::get(conn_lock!(context), query_organization.id)?.into_json())
}
