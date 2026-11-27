const fs=require('fs');
let t=fs.readFileSync('mod.rs','utf8');
t=t.replace('pub(crate) use action::{action_guard, job_action_parts};','pub(crate) use action::action_guard;');
t=t.replace('pub(crate) use rbac::{ns_from_api_path, rbac_guard};','pub(crate) use rbac::rbac_guard;');
fs.writeFileSync('mod.rs',t,'utf8');
console.log('re-exports trimmed');
