import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_IMPORT_BYTES, importProblem, nameProblem, parseDotenv, valueProblem } from "./dotenv-parse.ts";

const pairs = (text) => parseDotenv(text).entries.map((entry) => [entry.name, entry.value]);

test("plain and exported pairs", () => {
  assert.deepEqual(pairs("A=1\nexport B=two\n  export   C = three  "), [
    ["A", "1"],
    ["B", "two"],
    ["C", "three"],
  ]);
});

test("comments and blank lines are skipped", () => {
  const result = parseDotenv("# heading\n\n   \nA=1\n  # indented comment\nB=2");
  assert.deepEqual(result.entries.map((entry) => [entry.name, entry.value, entry.line]), [
    ["A", "1", 4],
    ["B", "2", 6],
  ]);
  assert.deepEqual(result.skipped, []);
});

test("trailing comments are stripped only after whitespace", () => {
  assert.deepEqual(pairs("A=value # note\nB=pass#word\nC=x\t# tab"), [
    ["A", "value"],
    ["B", "pass#word"],
    ["C", "x"],
  ]);
});

test("single quotes keep text literally", () => {
  assert.deepEqual(pairs("A='a\\nb # not a comment'\nB='$HOME \"x\"'"), [
    ["A", "a\\nb # not a comment"],
    ["B", '$HOME "x"'],
  ]);
});

test("double quotes understand escapes", () => {
  assert.deepEqual(pairs('A="line1\\nline2\\tend \\"q\\" back\\\\slash" # trailing'), [
    ["A", 'line1\nline2\tend "q" back\\slash'],
  ]);
});

test("quoted values can span lines", () => {
  const result = parseDotenv('KEY="-----BEGIN-----\nabc\n-----END-----"\nNEXT=1');
  assert.deepEqual(result.entries.map((entry) => [entry.name, entry.value, entry.line]), [
    ["KEY", "-----BEGIN-----\nabc\n-----END-----", 1],
    ["NEXT", "1", 4],
  ]);
});

test("CRLF line endings", () => {
  assert.deepEqual(pairs("A=1\r\nB=\"x\r\ny\"\r\nC=3\r\n"), [
    ["A", "1"],
    ["B", "x\ny"],
    ["C", "3"],
  ]);
});

test("duplicate keys keep the last value and are flagged", () => {
  const result = parseDotenv("A=first\nB=1\nA=second");
  assert.deepEqual(result.entries.map((entry) => [entry.name, entry.value, entry.duplicate, entry.line]), [
    ["B", "1", false, 2],
    ["A", "second", true, 3],
  ]);
});

test("invalid names carry a reason", () => {
  const result = parseDotenv("lower=1\nLOKA_X=1\nPATH=/bin\nGIT_DIR=x\n1BAD=1\nGOOD_1=ok\nEMPTY=");
  const problems = Object.fromEntries(result.entries.map((entry) => [entry.name, entry.problem]));
  assert.match(problems.lower, /capital letters/);
  assert.match(problems.LOKA_X, /reserved/);
  assert.match(problems.PATH, /Loka sets/);
  assert.match(problems.GIT_DIR, /Loka sets/);
  assert.match(problems["1BAD"], /start with a letter/);
  assert.equal(problems.GOOD_1, null);
  assert.match(problems.EMPTY, /empty/);
});

test("lines that are not pairs or never close are reported", () => {
  const result = parseDotenv("just text\nA=\"open\nB=1");
  assert.deepEqual(result.entries.map((entry) => entry.name), ["B"]);
  assert.deepEqual(result.skipped, [
    { line: 1, reason: "Not a NAME=value line." },
    { line: 2, reason: "The quote is never closed." },
  ]);
});

test("name and value limits match the server", () => {
  assert.equal(nameProblem("A".repeat(128)), null);
  assert.match(nameProblem("A".repeat(129)), /128/);
  assert.equal(nameProblem("DATABASE_URL"), null);
  assert.equal(valueProblem("x".repeat(64 * 1024)), null);
  assert.match(valueProblem("x".repeat(64 * 1024 + 1)), /64 KB/);
  assert.match(valueProblem("a\0b"), /null/);
});

test("byte order mark and equals signs inside values", () => {
  assert.deepEqual(pairs("﻿URL=postgres://u:p@h/db?a=b"), [["URL", "postgres://u:p@h/db?a=b"]]);
});

test("imports that are too large or too many are refused", () => {
  assert.equal(importProblem([{ name: "A", value: "x" }]), null);
  const many = Array.from({ length: 501 }, (_, i) => ({ name: `K_${i}`, value: "v" }));
  assert.match(importProblem(many), /at most 500/);
  const big = Array.from({ length: 200 }, (_, i) => ({ name: `K_${i}`, value: "x".repeat(MAX_IMPORT_BYTES / 150) }));
  assert.match(importProblem(big), /too large/);
});
