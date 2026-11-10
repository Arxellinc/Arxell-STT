import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
const source = await readFile(new URL("../ui/shortcut.js", import.meta.url), "utf8");
const { shortcutFromEvent } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
const key = (code, options = {}) => ({ code, key: code, ...options });

test("captures modifiers and native-supported key names", () => {
  assert.equal(shortcutFromEvent(key("Space", {ctrlKey:true, shiftKey:true})), "Ctrl+Shift+Space");
  assert.equal(shortcutFromEvent(key("KeyK", {metaKey:true})), "Super+K");
  assert.equal(shortcutFromEvent(key("Digit1", {altKey:true})), "Alt+1");
  assert.equal(shortcutFromEvent(key("Equal", {key:"+", ctrlKey:true, shiftKey:true})), "Ctrl+Shift+Equal");
  assert.equal(shortcutFromEvent(key("NumpadAdd", {ctrlKey:true})), "Ctrl+NumpadAdd");
  assert.equal(shortcutFromEvent(key("F24", {altKey:true})), "Alt+F24");
  assert.equal(shortcutFromEvent(key("ArrowUp", {ctrlKey:true})), "Ctrl+ArrowUp");
  assert.equal(shortcutFromEvent(key("Escape", {ctrlKey:true})), "Ctrl+Escape");
});
test("modifier presses and auto-repeat do not replace the candidate", () => {
  assert.equal(shortcutFromEvent(key("ControlLeft", {ctrlKey:true})), null);
  assert.equal(shortcutFromEvent(key("MetaRight", {metaKey:true})), null);
  assert.equal(shortcutFromEvent(key("KeyK", {ctrlKey:true, repeat:true})), null);
});
test("rejects bare keys, unsupported keys, dead keys, IME and AltGr", () => {
  for (const event of [key("KeyA"), key("F25", {ctrlKey:true}), key("IntlBackslash", {ctrlKey:true}),
    key("Quote", {key:"Dead", ctrlKey:true}), key("KeyA", {isComposing:true, ctrlKey:true}),
    key("KeyQ", {ctrlKey:true, altKey:true, getModifierState: name => name === "AltGraph"})]) {
    assert.throws(() => shortcutFromEvent(event));
  }
});
