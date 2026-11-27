const fs=require('fs');
const files=['connect.rs','objects.rs','discovery.rs','jobs.rs','runtime.rs','apply.rs','lib.rs'];
for(const f of files){
  let t=fs.readFileSync(f,'utf8');
  // 同类内的私有方法要能被兄弟模块调用：统一 pub(crate)
  t=t.replace(/^(    )(async )?fn /gm,'$1pub(crate) $2fn ');
  fs.writeFileSync(f,t,'utf8');
}
console.log('visibility opened');
