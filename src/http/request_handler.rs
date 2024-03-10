use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Result;
use async_graphql::http::{playground_source, GraphQLPlaygroundConfig};
use async_graphql::ServerError;
use hyper::header::{self, CONTENT_TYPE};
use hyper::http::Method;
use hyper::{Body, HeaderMap, Request, Response, StatusCode};
use prometheus::{Encoder, ProtobufEncoder, TextEncoder, PROTOBUF_FORMAT, TEXT_FORMAT};
use serde::de::DeserializeOwned;
use tracing::instrument;

use super::request_context::RequestContext;
use super::{showcase, AppContext};
use crate::async_graphql_hyper::{GraphQLRequestLike, GraphQLResponse};
use crate::blueprint::telemetry::TelemetryExporter;
use crate::blueprint::CorsParams;
use crate::config::{PrometheusExporter, PrometheusFormat};

const API_URL_PREFIX: &str = "/api";

pub fn graphiql(req: &Request<Body>) -> Result<Response<Body>> {
    let query = req.uri().query();
    let endpoint = "/graphql";
    let endpoint = if let Some(query) = query {
        if query.is_empty() {
            Cow::Borrowed(endpoint)
        } else {
            Cow::Owned(format!("{}?{}", endpoint, query))
        }
    } else {
        Cow::Borrowed(endpoint)
    };

    Ok(Response::new(Body::from(playground_source(
        GraphQLPlaygroundConfig::new(&endpoint).title("Tailcall - GraphQL IDE"),
    ))))
}

fn prometheus_metrics(prometheus_exporter: &PrometheusExporter) -> Result<Response<Body>> {
    let metric_families = prometheus::default_registry().gather();
    let mut buffer = vec![];

    match prometheus_exporter.format {
        PrometheusFormat::Text => TextEncoder::new().encode(&metric_families, &mut buffer)?,
        PrometheusFormat::Protobuf => {
            ProtobufEncoder::new().encode(&metric_families, &mut buffer)?
        }
    };

    let content_type = match prometheus_exporter.format {
        PrometheusFormat::Text => TEXT_FORMAT,
        PrometheusFormat::Protobuf => PROTOBUF_FORMAT,
    };

    Ok(Response::builder()
        .status(200)
        .header(CONTENT_TYPE, content_type)
        .body(Body::from(buffer))?)
}

fn not_found() -> Result<Response<Body>> {
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())?)
}

fn create_request_context(req: &Request<Body>, app_ctx: &AppContext) -> RequestContext {
    let upstream = app_ctx.blueprint.upstream.clone();
    let allowed = upstream.allowed_headers;
    let headers = create_allowed_headers(req.headers(), &allowed);
    RequestContext::from(app_ctx).req_headers(headers)
}

fn update_cache_control_header(
    response: GraphQLResponse,
    app_ctx: &AppContext,
    req_ctx: Arc<RequestContext>,
) -> GraphQLResponse {
    if app_ctx.blueprint.server.enable_cache_control_header {
        let ttl = req_ctx.get_min_max_age().unwrap_or(0);
        let cache_public_flag = req_ctx.is_cache_public().unwrap_or(true);
        return response.set_cache_control(ttl, cache_public_flag);
    }
    response
}

pub fn update_response_headers(resp: &mut hyper::Response<hyper::Body>, app_ctx: &AppContext) {
    if !app_ctx.blueprint.server.response_headers.is_empty() {
        resp.headers_mut()
            .extend(app_ctx.blueprint.server.response_headers.clone());
    }
}

pub async fn graphql_request<T: DeserializeOwned + GraphQLRequestLike>(
    req: Request<Body>,
    app_ctx: &AppContext,
) -> Result<Response<Body>> {
    let req_ctx = Arc::new(create_request_context(&req, app_ctx));
    let bytes = hyper::body::to_bytes(req.into_body()).await?;
    let graphql_request = serde_json::from_slice::<T>(&bytes);
    match graphql_request {
        Ok(request) => {
            let mut response = request.data(req_ctx.clone()).execute(&app_ctx.schema).await;
            response = update_cache_control_header(response, app_ctx, req_ctx);
            let mut resp = response.to_response()?;
            update_response_headers(&mut resp, app_ctx);
            Ok(resp)
        }
        Err(err) => {
            tracing::error!(
                "Failed to parse request: {}",
                String::from_utf8(bytes.to_vec()).unwrap()
            );

            let mut response = async_graphql::Response::default();
            let server_error =
                ServerError::new(format!("Unexpected GraphQL Request: {}", err), None);
            response.errors = vec![server_error];

            Ok(GraphQLResponse::from(response).to_response()?)
        }
    }
}

fn create_allowed_headers(headers: &HeaderMap, allowed: &BTreeSet<String>) -> HeaderMap {
    let mut new_headers = HeaderMap::new();
    for (k, v) in headers.iter() {
        if allowed.contains(k.as_str()) {
            new_headers.insert(k, v.clone());
        }
    }

    new_headers
}

fn ensure_usable_cors_rules(layer: &CorsParams) {
    if layer.allow_credentials {
        assert!(
            !layer.allow_headers.is_wildcard(),
            "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
             with `Access-Control-Allow-Headers: *`"
        );

        assert!(
            !layer.allow_methods.is_wildcard(),
            "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
             with `Access-Control-Allow-Methods: *`"
        );

        assert!(
            !layer.allow_origin.is_wildcard(),
            "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
             with `Access-Control-Allow-Origin: *`"
        );

        assert!(
            !layer.expose_headers_is_wildcard(),
            "Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
             with `Access-Control-Expose-Headers: *`"
        );
    }
}

pub async fn handle_request_with_cors<T: DeserializeOwned + GraphQLRequestLike>(
    req: Request<Body>,
    cors: &CorsParams,
    app_ctx: Arc<AppContext>,
) -> Result<Response<Body>> {
    ensure_usable_cors_rules(cors);
    let (parts, body) = req.into_parts();
    let origin = parts.headers.get(&header::ORIGIN);

    let mut headers = HeaderMap::new();

    // These headers are applied to both preflight and subsequent regular CORS
    // requests: https://fetch.spec.whatwg.org/#http-responses

    headers.extend(cors.allow_origin_to_header(origin));
    headers.extend(cors.allow_credentials_to_header());
    headers.extend(cors.allow_private_network_to_header(&parts));
    headers.extend(cors.vary_to_header());

    // Return results immediately upon preflight request
    if parts.method == Method::OPTIONS {
        // These headers are applied only to preflight requests
        headers.extend(cors.allow_methods_to_header(&parts));
        headers.extend(cors.allow_headers_to_header(&parts));
        headers.extend(cors.max_age_to_header());

        let mut response = Response::new(Body::default());
        std::mem::swap(response.headers_mut(), &mut headers);

        Ok(response)
    } else {
        // This header is applied only to non-preflight requests
        headers.extend(cors.expose_headers_to_header());

        let req = Request::from_parts(parts, body);
        let mut response = handle_request::<T>(req, app_ctx).await?;

        let response_headers = response.headers_mut();

        // vary header can have multiple values, don't overwrite
        // previously-set value(s).
        if let Some(vary) = headers.remove(header::VARY) {
            response_headers.append(header::VARY, vary);
        }
        // extend will overwrite previous headers of remaining names
        response_headers.extend(headers.drain());

        Ok(response)
    }
}

async fn handle_rest_apis(
    mut request: Request<Body>,
    app_ctx: Arc<AppContext>,
) -> Result<Response<Body>> {
    *request.uri_mut() = request.uri().path().replace(API_URL_PREFIX, "").parse()?;
    let req_ctx = Arc::new(create_request_context(&request, app_ctx.as_ref()));
    if let Some(p_request) = app_ctx.endpoints.matches(&request) {
        let graphql_request = p_request.into_request(request).await?;
        let mut response = graphql_request
            .data(req_ctx.clone())
            .execute(&app_ctx.schema)
            .await;
        response = update_cache_control_header(response, app_ctx.as_ref(), req_ctx);
        let mut resp = response.to_response()?;
        update_response_headers(&mut resp, app_ctx.as_ref());
        return Ok(resp);
    }

    not_found()
}

#[instrument(skip_all, err, fields(method = %req.method(), url = %req.uri()))]
pub async fn handle_request<T: DeserializeOwned + GraphQLRequestLike>(
    req: Request<Body>,
    app_ctx: Arc<AppContext>,
) -> Result<Response<Body>> {
    if req.uri().path().starts_with(API_URL_PREFIX) {
        return handle_rest_apis(req, app_ctx).await;
    }

    match *req.method() {
        // NOTE:
        // The first check for the route should be for `/graphql`
        // This is always going to be the most used route.
        hyper::Method::POST if req.uri().path() == "/graphql" => {
            graphql_request::<T>(req, app_ctx.as_ref()).await
        }
        hyper::Method::POST
            if app_ctx.blueprint.server.enable_showcase
                && req.uri().path() == "/showcase/graphql" =>
        {
            let app_ctx =
                match showcase::create_app_ctx::<T>(&req, app_ctx.runtime.clone(), false).await? {
                    Ok(app_ctx) => app_ctx,
                    Err(res) => return Ok(res),
                };

            graphql_request::<T>(req, &app_ctx).await
        }

        hyper::Method::GET => {
            if let Some(TelemetryExporter::Prometheus(prometheus)) =
                app_ctx.blueprint.opentelemetry.export.as_ref()
            {
                if req.uri().path() == prometheus.path {
                    return prometheus_metrics(prometheus);
                }
            };

            if app_ctx.blueprint.server.enable_graphiql {
                return graphiql(&req);
            }

            not_found()
        }
        _ => not_found(),
    }
}
