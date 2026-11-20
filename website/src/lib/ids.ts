// Ids for markup that a page can render more than once (the mark in the
// header and the footer). Frontmatter runs per render, so the count lives here.
let next = 0;

export function uniqueId(prefix: string): string {
  next += 1;
  return `${prefix}${next}`;
}
