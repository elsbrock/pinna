// Version precedence for release channels (ADR 0022).
type Identifier = string | number;
type Version = { major: Identifier; minor: Identifier; patch: Identifier; pre: Identifier[] };

// Keep large integers as decimal strings so adjacent versions stay distinct.
function integer(s: string): Identifier {
  const n = Number(s);
  return Number.isSafeInteger(n) ? n : s;
}

export function parse(v: string): Version | null {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/.exec(v);
  if (!match) return null;
  const ids = match[4]?.split('.') ?? [];
  if (ids.some(id => /^0\d/.test(id) && /^\d+$/.test(id))) return null;
  return {
    major: integer(match[1]), minor: integer(match[2]), patch: integer(match[3]),
    pre: ids.map(id => /^\d+$/.test(id) ? integer(id) : id),
  };
}

function compareId(a: Identifier, b: Identifier): number {
  const x = String(a), y = String(b);
  const nx = /^\d+$/.test(x), ny = /^\d+$/.test(y);
  if (nx !== ny) return nx ? -1 : 1;
  if (nx && x.length !== y.length) return x.length - y.length;
  return x < y ? -1 : x > y ? 1 : 0;
}

export function compare(a: Version, b: Version): number {
  const core = compareId(a.major, b.major) || compareId(a.minor, b.minor) || compareId(a.patch, b.patch);
  if (core) return core;
  if (!a.pre.length || !b.pre.length) return b.pre.length - a.pre.length;
  for (let i = 0; i < Math.min(a.pre.length, b.pre.length); i++) {
    const order = compareId(a.pre[i], b.pre[i]);
    if (order) return order;
  }
  return a.pre.length - b.pre.length;
}
