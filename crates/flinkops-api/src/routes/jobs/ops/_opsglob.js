const fs=require('fs');
for(const f of ['bluegreen.rs','ingress.rs','ops_actions.rs']){
  let t=fs.readFileSync(f,'utf8');
  t=t.replace('use super::*;','use super::super::*;');
  fs.writeFileSync(f,t,'utf8');
}
console.log('glob path fixed');
