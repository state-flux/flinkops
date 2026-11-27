const fs=require('fs');
const files=['action.rs','audit.rs','csrf.rs','error.rs','rbac.rs'];
for(const f of files){
  let t=fs.readFileSync(f,'utf8');
  // 子模块的父是 middleware，不再是 routes：改用 crate:: 绝对路径导入
  if(!t.includes('use crate::routes::')){
    t=t.replace('use super::*;', 'use std::sync::Arc;\n\nuse crate::state::AppState;');
  }
  fs.writeFileSync(f,t,'utf8');
}
console.log('imports rewritten');
