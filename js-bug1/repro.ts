// Bug 1 in loro-crdt (WASM): same 6-op scenario as ../bug1-tree-overflow.
// Run: bun install && bun repro.ts
import { LoroDoc, type Frontiers } from "loro-crdt";

type Scenario = { doc: LoroDoc; target: Frontiers; n0: string; n1: string };

function scenario(): Scenario {
  const a = new LoroDoc();
  a.setPeerId("1");
  const b = new LoroDoc();
  b.setPeerId("2");
  const ta = a.getTree("tree");
  const tb = b.getTree("tree");

  const n0 = ta.createNode().id;
  const n1 = ta.createNode().id;
  a.commit();
  b.import(a.export({ mode: "update" }));

  ta.move(n1, n0); // A@2
  a.commit();
  const aUpTo2 = a.export({ mode: "update" });

  tb.move(n0, undefined); // B@0: n0 -> root (lamport bump)
  b.commit();
  tb.move(n0, n1); // B@1: n0 under n1
  b.commit();
  b.import(aUpTo2); // B is now at [A@2, B@1]

  ta.move(n1, undefined); // A@3: n1 -> root
  a.commit();
  a.import(b.export({ mode: "update" }));

  const target: Frontiers = [
    { peer: "1", counter: 2 },
    { peer: "2", counter: 1 },
  ];
  return { doc: a, target, n0, n1 };
}

function render(doc: LoroDoc, s: Scenario): string {
  const name = (id: string) => (id === s.n0 ? "n0" : id === s.n1 ? "n1" : id);
  const walk = (nodes: ReturnType<ReturnType<LoroDoc["getTree"]>["roots"]>): string =>
    nodes
      .map((n) => {
        const kids = n.children() ?? [];
        return kids.length ? `${name(n.id)} { ${walk(kids)} }` : name(n.id);
      })
      .join(", ");
  return `root { ${walk(doc.getTree("tree").roots())} }`;
}

const probes: Record<string, (s: Scenario) => string> = {
  forkAt: (s) => render(s.doc.forkAt(s.target), s),
  checkout: (s) => {
    s.doc.checkout(s.target);
    return render(s.doc, s);
  },
  revertTo: (s) => {
    s.doc.revertTo(s.target);
    return render(s.doc, s);
  },
  diff: (s) => `${s.doc.diff(s.doc.frontiers(), s.target, false).length} container diff(s)`,
  // (loro-crdt's export() has no "snapshot-at" mode, so that API is Rust-only.)
  updatesTill: (s) => {
    const vv = s.doc.frontiersToVV(s.target);
    const e = new LoroDoc();
    e.import(s.doc.export({ mode: "updates-in-range", spans: vvSpans(vv.toJSON()) }));
    return render(e, s);
  },
};

function vvSpans(vv: Map<string, number>) {
  return [...vv.entries()].map(([peer, len]) => ({ id: { peer: peer as `${number}`, counter: 0 }, len }));
}

// Each probe runs in its own bun process: a WASM stack overflow corrupts the
// module's linear memory, so every later call in the same process fails too.
const only = process.argv[2];
if (only) {
  const s = scenario();
  try {
    console.log(`${only}: ${probes[only](s)}`);
  } catch (e) {
    const msg = e instanceof Error ? `${e.name}: ${e.message}` : String(e);
    console.log(`${only}: THREW ${msg.split("\n")[0].slice(0, 120)}`);
    try {
      new LoroDoc();
      console.log("   new LoroDoc() afterwards: ok");
    } catch (e2) {
      console.log(`   new LoroDoc() afterwards: THREW ${(e2 as Error).message.split("\n")[0].slice(0, 100)}`);
    }
  }
} else {
  const s0 = scenario();
  const version = (await import("loro-crdt/package.json", { with: { type: "json" } })).default.version;
  console.log(`loro-crdt ${version}`);
  console.log(`final tree: ${render(s0.doc, s0)}; expected at target: root { n0 { n1 } }`);
  for (const name of Object.keys(probes)) {
    const r = Bun.spawnSync([process.execPath, import.meta.path, name]);
    const out = r.stdout.toString().trim();
    console.log(out || `${name}: exit ${r.exitCode} ${r.stderr.toString().split("\n").find((l) => l.includes("Error")) ?? ""}`);
  }
}
