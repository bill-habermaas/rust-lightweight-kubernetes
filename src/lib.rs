//! lkube — a lightweight Kubernetes-like orchestrator.
//!
//! Layers:
//! * [`pb`]           — generated gRPC types (proto/lkube.proto)
//! * [`server`]       — gRPC service implementations (authn + authz on every command)
//! * [`auth`]/[`authz`]/[`security`] — JWT tokens, RBAC rules, request guards
//! * [`api`]          — validation, defaulting, cascading delete, watch events
//! * [`store`]        — persistence (MongoDB, plus an in-memory store for tests)
//! * [`operator`]     — the operator interface + manager that runs reconcile loops
//! * [`controllers`]  — built-in operators: scheduler, deployments, services, kubelet
//! * [`runtime`]      — container runtimes (Docker via bollard, and a mock)
//! * [`sdk`]          — client helpers for writing out-of-process operators

#![allow(clippy::result_large_err)]

pub mod pb {
    tonic::include_proto!("lkube.v1");
}

pub mod api;
pub mod auth;
pub mod authz;
pub mod bootstrap;
pub mod config;
pub mod controllers;
pub mod convert;
pub mod error;
pub mod model;
pub mod operator;
pub mod runtime;
pub mod sdk;
pub mod security;
pub mod server;
pub mod store;
