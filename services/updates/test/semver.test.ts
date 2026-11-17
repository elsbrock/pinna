import assert from 'node:assert/strict';
import test from 'node:test';
import { compare, parse } from '../src/semver.ts';

test('SemVer precedence for release, beta and development versions', () => {
  const ordered = ['0.4.0-beta.1', '0.4.0-beta.2', '0.4.0-beta.10', '0.4.0',
    '0.4.1-dev.202611171405.gabc1234', '0.4.1', '1.0.0-1', '1.0.0-alpha',
    '1.0.0-alpha.1', '1.0.0-beta', '1.0.0', '1.1.0', '2.0.0',
    '9007199254740992.0.0', '9007199254740993.0.0'];
  assert.deepEqual(parse('0.4.0-beta.2'), { major: 0, minor: 4, patch: 0, pre: ['beta', 2] });
  for (const [i, a] of ordered.entries()) {
    for (const [j, b] of ordered.entries()) {
      assert.equal(Math.sign(compare(parse(a)!, parse(b)!)), Math.sign(i - j), `${a} versus ${b}`);
    }
  }
});

test('invalid versions and build metadata are rejected', () => {
  for (const v of ['v1.0.0', '1.0', '01.0.0', '1.0.0+build', '1.0.0-01', '1.0.0-', '1.0.0-a..b']) {
    assert.equal(parse(v), null, v);
  }
});
