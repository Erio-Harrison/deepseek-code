// Webview UI. Everything is set via textContent except finished assistant
// replies, which go through renderMarkdown().
(function () {
  const vscode = acquireVsCodeApi();
  const tabsEl = document.getElementById("tabs");
  const listEl = document.getElementById("list");
  const inputEl = document.getElementById("input");
  const sendBtn = document.getElementById("send");
  const stopBtn = document.getElementById("stop");
  const modeEl = document.getElementById("mode");
  const modelEl = document.getElementById("model");

  const MODE_LABELS = { manual: "Manual", acceptEdits: "Auto-accept edits" };

  let sessions = [];
  let activeId = "";
  let itemEls = [];
  let config = { mode: "manual", model: "", models: [] };

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function activeStatus() {
    const s = sessions.find((x) => x.id === activeId);
    return s ? s.status : "idle";
  }

  function pretty(value) {
    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return String(value);
    }
  }

  // marked passes raw HTML through, so DOMPurify is required here. The
  // forbidden tags would otherwise load remote content on render.
  const SANITIZE_OPTS = { FORBID_TAGS: ["img", "iframe", "object", "embed", "style"] };
  function renderMarkdown(container, text) {
    const html = DOMPurify.sanitize(marked.parse(text, { breaks: true, gfm: true }), SANITIZE_OPTS);
    container.innerHTML = html;
  }

  function renderTabs() {
    tabsEl.replaceChildren();
    for (const s of sessions) {
      const tab = el("div", "tab" + (s.id === activeId ? " active" : ""));
      tab.appendChild(el("span", "dot " + s.status));
      tab.appendChild(el("span", "title", s.title));
      const close = el("button", "close", "×");
      close.title = "Close session";
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        vscode.postMessage({ type: "closeSession", id: s.id });
      });
      tab.appendChild(close);
      tab.addEventListener("click", () =>
        vscode.postMessage({ type: "switchSession", id: s.id }),
      );
      tabsEl.appendChild(tab);
    }
    const add = el("button", "add", "+");
    add.title = "New session";
    add.addEventListener("click", () => vscode.postMessage({ type: "newSession" }));
    tabsEl.appendChild(add);
  }

  function renderControls() {
    const status = activeStatus();
    const busy = status === "running" || status === "awaiting";
    sendBtn.disabled = busy || inputEl.value.trim() === "";
    stopBtn.hidden = !busy;
  }

  // Readable previews for known tools; anything else is pretty JSON.
  function toolPreview(toolName, input) {
    if (
      toolName === "write_file" &&
      input &&
      typeof input === "object" &&
      typeof input.path === "string" &&
      typeof input.content === "string"
    ) {
      return { label: input.path, body: input.content };
    }
    if (toolName === "bash" && input && typeof input === "object" && typeof input.command === "string") {
      return { label: null, body: "$ " + input.command };
    }
    return { label: null, body: pretty(input) };
  }

  function appendPreview(card, toolName, input) {
    const preview = toolPreview(toolName, input);
    if (preview.label) {
      card.appendChild(el("div", "preview-label", preview.label));
    }
    card.appendChild(el("pre", "", preview.body));
  }

  function renderItem(item) {
    switch (item.kind) {
      case "user":
        return el("div", "msg user", item.text);
      case "assistant": {
        const div = el("div", "msg assistant" + (item.streaming ? " streaming" : ""));
        if (item.streaming) {
          // Plain text while streaming; the final patch renders Markdown.
          div.textContent = item.text;
        } else {
          renderMarkdown(div, item.text);
        }
        return div;
      }
      case "tool_use": {
        const card = el("details", "card");
        card.appendChild(el("summary", "", "Tool call: " + item.name));
        appendPreview(card, item.name, item.input);
        return card;
      }
      case "tool_result": {
        const card = el("details", "card" + (item.isError ? " error" : ""));
        card.appendChild(el("summary", "", item.isError ? "Tool error" : "Tool result"));
        card.appendChild(el("pre", "", item.content));
        return card;
      }
      case "permission":
        return renderPermission(item);
      default:
        return el("div", "notice" + (item.isError ? " error" : ""), item.text);
    }
  }

  function renderPermission(item) {
    const card = el("div", "card permission" + (item.escalated ? " escalated" : ""));
    card.appendChild(
      el(
        "div",
        "ask",
        item.escalated
          ? "⚠ Run \"" + item.toolName + "\" OUTSIDE the sandbox?"
          : "Allow tool \"" + item.toolName + "\"?",
      ),
    );
    const reason = item.input && typeof item.input === "object" ? item.input.reason : undefined;
    if (item.escalated && typeof reason === "string" && reason) {
      card.appendChild(el("div", "reason", reason));
    }
    appendPreview(card, item.toolName, item.input);
    const row = el("div", "row");
    if (item.auto) {
      row.appendChild(el("span", "status allowed", "Auto-approved (" + MODE_LABELS[config.mode] + ")"));
    } else if (item.status === "pending") {
      const allow = el("button", "", "Allow");
      allow.addEventListener("click", () =>
        vscode.postMessage({ type: "permission", requestId: item.requestId, allow: true }),
      );
      row.appendChild(allow);
      if (item.escalated) {
        const allowSession = el("button", "secondary", "Allow (this session)");
        allowSession.title = "Don't ask again for this kind of escalation until the session ends";
        allowSession.addEventListener("click", () =>
          vscode.postMessage({
            type: "permission",
            requestId: item.requestId,
            allow: true,
            remember: "session",
          }),
        );
        row.appendChild(allowSession);
      }
      const deny = el("button", "danger", "Deny");
      deny.addEventListener("click", () =>
        vscode.postMessage({ type: "permission", requestId: item.requestId, allow: false }),
      );
      row.appendChild(deny);
    } else {
      row.appendChild(el("span", "status " + item.status, item.status === "allowed" ? "Allowed" : "Denied"));
    }
    if (item.hasDiff) {
      const diff = el("button", "secondary", "View diff");
      diff.addEventListener("click", () =>
        vscode.postMessage({ type: "showDiff", requestId: item.requestId }),
      );
      row.appendChild(diff);
    }
    card.appendChild(row);
    return card;
  }

  function renderConfig() {
    modeEl.replaceChildren(
      ...Object.entries(MODE_LABELS).map(([value, label]) => {
        const o = document.createElement("option");
        o.value = value;
        o.textContent = label;
        return o;
      }),
    );
    modeEl.value = config.mode;

    modelEl.replaceChildren(
      ...config.models.map((m) => {
        const o = document.createElement("option");
        o.value = m;
        o.textContent = m;
        return o;
      }),
    );
    modelEl.value = config.model;
  }

  function nearBottom() {
    return listEl.scrollHeight - listEl.scrollTop - listEl.clientHeight < 40;
  }

  function withAutoScroll(mutate) {
    const stick = nearBottom();
    mutate();
    if (stick) listEl.scrollTop = listEl.scrollHeight;
  }

  function renderAll(items) {
    listEl.replaceChildren();
    itemEls = items.map((item) => {
      const node = renderItem(item);
      listEl.appendChild(node);
      return node;
    });
    listEl.scrollTop = listEl.scrollHeight;
  }

  function applyPatch(index, item) {
    withAutoScroll(() => {
      const node = renderItem(item);
      if (index < itemEls.length) {
        listEl.replaceChild(node, itemEls[index]);
        itemEls[index] = node;
      } else {
        listEl.appendChild(node);
        itemEls.push(node);
      }
    });
  }

  function send() {
    const text = inputEl.value;
    if (sendBtn.disabled || text.trim() === "") return;
    vscode.postMessage({ type: "send", text });
    inputEl.value = "";
    renderControls();
  }

  sendBtn.addEventListener("click", send);
  stopBtn.addEventListener("click", () => vscode.postMessage({ type: "stop" }));
  modeEl.addEventListener("change", () => vscode.postMessage({ type: "setMode", mode: modeEl.value }));
  modelEl.addEventListener("change", () => vscode.postMessage({ type: "setModel", model: modelEl.value }));
  inputEl.addEventListener("input", renderControls);
  inputEl.addEventListener("keydown", (e) => {
    // Skip Enter while an IME (e.g. Chinese input) is composing.
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing && e.keyCode !== 229) {
      e.preventDefault();
      send();
    }
  });

  window.addEventListener("message", (event) => {
    const m = event.data;
    switch (m.type) {
      case "state":
        sessions = m.sessions;
        activeId = m.activeId;
        renderTabs();
        renderAll(m.items);
        renderControls();
        break;
      case "sessions":
        sessions = m.sessions;
        activeId = m.activeId;
        renderTabs();
        renderControls();
        break;
      case "patch":
        if (m.sessionId === activeId) applyPatch(m.index, m.item);
        break;
      case "append":
        if (m.sessionId === activeId && itemEls[m.index]) {
          withAutoScroll(() => {
            itemEls[m.index].textContent += m.text;
          });
        }
        break;
      case "config":
        config = { mode: m.mode, model: m.model, models: m.models };
        renderConfig();
        break;
    }
  });

  vscode.postMessage({ type: "ready" });
})();
