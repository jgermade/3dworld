// The wasm half of `make xarch`: instantiate the module and print its rows.
//
// There is no wasm-bindgen JS glue here and none is generated. The three
// exports take and return numbers, so the raw module is enough — which keeps
// this check to `rustup target add wasm32-unknown-unknown` and a node, rather
// than to a `cargo install wasm-bindgen-cli` that would mean it mostly does not
// run. The price is the name: wasm-bindgen appends a hash to an export until
// its CLI strips one, so a row is found by prefix and an ambiguous prefix is an
// error rather than a guess.
//
// Every import is stubbed with a function that throws. Nothing in the
// measurement should reach into JS, and a run where something did is a run
// whose numbers mean nothing — so it fails loudly instead of returning them.

const fs = require('fs');

const path = process.argv[2];
if (!path) {
  console.error('usage: node tools/xarch.js <module.wasm>');
  process.exit(2);
}

const mod = new WebAssembly.Module(fs.readFileSync(path));

const imports = {};
for (const imp of WebAssembly.Module.imports(mod)) {
  imports[imp.module] ??= {};
  imports[imp.module][imp.name] = (...args) => {
    throw new Error(`the measurement called into JS: ${imp.module}.${imp.name}(${args})`);
  };
}

const instance = new WebAssembly.Instance(mod, imports);

function exported(name) {
  const keys = Object.keys(instance.exports).filter(
    (k) => k === name || k.startsWith(`${name}_`),
  );
  if (keys.length !== 1) {
    throw new Error(`export "${name}": expected one match, found ${keys.length} [${keys}]`);
  }
  return instance.exports[keys[0]];
}

const prepare = exported('prepare');
const value = exported('value');
const rule = exported('rule');

const RULES = ['exact', 'close', 'report'];
const bits = new DataView(new ArrayBuffer(8));

const count = prepare();
for (let i = 0; i < count; i++) {
  const v = value(i);
  bits.setFloat64(0, v);
  let hex = '';
  for (let j = 0; j < 8; j++) hex += bits.getUint8(j).toString(16).padStart(2, '0');
  const r = RULES[rule(i)] ?? `rule#${rule(i)}`;
  console.log(`${i}\t${r}\t${hex}\t${v}`);
}
