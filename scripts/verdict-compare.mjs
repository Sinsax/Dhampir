#!/usr/bin/env node
// 判定回传里用到的两个比较器：**纯函数，零依赖**。
//
// 为什么要单独一个模块：它们决定「预览与 CLI 是不是给了同一个工程」这句话的真假，
// 而这句话是 P7 里唯一一条此前没能成立的判据。判定逻辑必须能单独自证 ——
// 埋在驱动里的话，只能靠"跑一次浏览器"来验它，而那条路又慢又容易受环境影响。
//
// 用法：
//   node scripts/verdict-compare.mjs --self-test

import { pathToFileURL } from 'node:url';

/**
 * 只看值、不看键序的深比较。返回第一处不同（完全相同返回 null）。
 * 报**路径**而不是只报「不一样」—— 结论要能直接定位。
 */
export function firstDifference(left, right, path) {
  const leftObject = left !== null && typeof left === 'object';
  const rightObject = right !== null && typeof right === 'object';
  if (!leftObject || !rightObject) {
    return left === right ? null : path + '：' + JSON.stringify(left) + ' != ' + JSON.stringify(right);
  }
  if (Array.isArray(left) !== Array.isArray(right)) return path + '：一边是数组一边不是';
  if (Array.isArray(left)) {
    if (left.length !== right.length) return path + '：长度 ' + left.length + ' != ' + right.length;
    for (let index = 0; index < left.length; index += 1) {
      const found = firstDifference(left[index], right[index], path + '[' + index + ']');
      if (found !== null) return found;
    }
    return null;
  }
  const keys = Object.keys(left).sort();
  const other = Object.keys(right).sort();
  if (keys.join(',') !== other.join(',')) {
    return path + '：键不同 [' + keys.join(',') + '] != [' + other.join(',') + ']';
  }
  for (const key of keys) {
    const found = firstDifference(left[key], right[key], path + '.' + key);
    if (found !== null) return found;
  }
  return null;
}

export function deepEqual(left, right) {
  return firstDifference(left, right, '$') === null;
}

/**
 * expected 里**写了的**字段必须在 actual 里逐字段相同。
 *
 * 为什么不是全等：工程文件里省略了取默认值的字段（blend/effects/transform…），
 * 而宿主里那份是**引擎重新序列化**出来的，把这些字段补齐了。
 * 于是「文件 ⊑ 宿主」才是能成立的对照；反过来比会把默认字段当成差异。
 */
export function firstSubsetDifference(expected, actual, path) {
  const expectedObject = expected !== null && typeof expected === 'object';
  if (!expectedObject) {
    return expected === actual ? null : path + '：' + JSON.stringify(expected) + ' != ' + JSON.stringify(actual);
  }
  if (Array.isArray(expected)) {
    if (!Array.isArray(actual)) return path + '：期望是数组，实际不是';
    if (expected.length !== actual.length) return path + '：长度 ' + expected.length + ' != ' + actual.length;
    for (let index = 0; index < expected.length; index += 1) {
      const found = firstSubsetDifference(expected[index], actual[index], path + '[' + index + ']');
      if (found !== null) return found;
    }
    return null;
  }
  for (const key of Object.keys(expected)) {
    if (!(key in actual)) return path + '.' + key + '：宿主那份里没有这个键';
    const found = firstSubsetDifference(expected[key], actual[key], path + '.' + key);
    if (found !== null) return found;
  }
  return null;
}

function runSelfTest() {
  let passed = 0;
  const expect = (name, condition) => {
    if (!condition) throw new Error('自检失败：' + name);
    passed += 1;
  };

  expect('相同对象相等', deepEqual({ a: 1, b: [1, 2] }, { a: 1, b: [1, 2] }));
  expect('键序不是语义', deepEqual({ a: 1, b: 2 }, { b: 2, a: 1 }));
  expect('数字与字符串不等', firstDifference(1, '1', '$') !== null);
  expect('缺键要红', firstDifference({ a: 1 }, { b: 1 }, '$').includes('键不同'));
  expect('值不同要红且给出路径', firstDifference({ a: { b: 1 } }, { a: { b: 2 } }, '$').includes('.a.b'));
  expect('数组长度不同要红', firstDifference([1, 2], [1, 2, 3], '$').includes('长度'));
  expect('数组元素不同要给出下标', firstDifference([1], [2], '$').includes('[0]'));
  expect('null 与 null 相等', deepEqual(null, null));

  expect('子集：宿主多出来的默认字段不算差异', firstSubsetDifference({ a: 1 }, { a: 1, blend: 'normal' }) === null);
  expect('子集：宿主少了一个文件里写着的键 -> 红', firstSubsetDifference({ a: 1 }, { b: 1 }) !== null);
  expect('子集：文件里写了但值不同 -> 红', firstSubsetDifference({ a: 1 }, { a: 2 }) !== null);
  expect('子集：数组元素逐个比', firstSubsetDifference([{ x: 1 }], [{ x: 1, y: 2 }]) === null);
  expect('子集：数组长度不同 -> 红', firstSubsetDifference([1], [1, 2]) !== null);
  expect('子集：空对象通过', firstSubsetDifference({}, { anything: 1 }) === null);

  console.log('OK 判定比较器自检通过（' + passed + ' 条断言）');
}

function main() {
  const args = process.argv.slice(2);
  if (args.indexOf('--self-test') >= 0) { runSelfTest(); return 0; }
  if (args.indexOf('--help') >= 0 || args.indexOf('-h') >= 0) {
    console.log('用法：node scripts/verdict-compare.mjs --self-test');
    return 0;
  }
  console.log('这两个比较器是给 scripts/web-check.mjs 的 --verdict 用的；自带 --self-test。');
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main();
}
