const fs=require('fs');
let t=fs.readFileSync('error.rs','utf8');
if(!t.includes('use axum::http::header')){
  t=t.replace('use crate::state::AppState;','use crate::state::AppState;\nuse axum::http::header;');
  fs.writeFileSync('error.rs',t,'utf8');
  console.log('header import added');
}
