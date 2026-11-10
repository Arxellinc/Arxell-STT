// Local-only transcript storage. IndexedDB avoids localStorage's small quota
// and synchronous whole-history rewrites. Unlimited history is paged in the UI.
let database;
function open() {
  if (!database) database = new Promise((resolve, reject) => {
    const request = indexedDB.open("arxell-transcripts", 1);
    request.onupgradeneeded = () => {
      request.result.createObjectStore("history", { autoIncrement: true });
      request.result.createObjectStore("draft");
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(new Error("Close other app windows to upgrade transcript storage."));
  });
  return database;
}
async function transaction(stores, mode, run) {
  const db = await open();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(stores, mode);
    let result;
    tx.oncomplete = () => resolve(result);
    tx.onerror = tx.onabort = () => reject(tx.error || new Error("Transcript storage failed."));
    run(tx, (value) => { result = value; });
  });
}
function prune(store, limit) {
  if (!limit) return;
  const count = store.count();
  count.onsuccess = () => {
    let excess = count.result - limit;
    if (excess <= 0) return;
    const cursor = store.openCursor();
    cursor.onsuccess = () => {
      if (!cursor.result || excess-- <= 0) return;
      cursor.result.delete();
      cursor.result.continue();
    };
  };
}
export function clearHistory() {
  return transaction(["history"], "readwrite", (tx) => tx.objectStore("history").clear());
}
export function trimHistory(limit) {
  return transaction(["history"], "readwrite", (tx) => prune(tx.objectStore("history"), limit));
}
export function archiveTranscript(entry, limit) {
  return transaction(["history", "draft"], "readwrite", (tx) => {
    const store = tx.objectStore("history");
    store.add(entry);
    prune(store, limit);
    tx.objectStore("draft").delete("current");
  });
}
export function saveDraft(entry) {
  return transaction(["draft"], "readwrite", (tx) => tx.objectStore("draft").put(entry, "current"));
}
export function loadDraft() {
  return transaction(["draft"], "readonly", (tx, result) => {
    const request = tx.objectStore("draft").get("current");
    request.onsuccess = () => result(request.result);
  });
}
export function loadHistory(count) {
  return transaction(["history"], "readonly", (tx, result) => {
    const entries = [];
    const request = tx.objectStore("history").openCursor(null, "prev");
    request.onsuccess = () => {
      const cursor = request.result;
      if (!cursor || entries.length === count) {
        result({ entries: entries.reverse(), hasMore: Boolean(cursor) });
        return;
      }
      entries.push({ ...cursor.value, id: cursor.key });
      cursor.continue();
    };
  });
}
