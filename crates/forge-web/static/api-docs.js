// API docs page: syntax highlighting of the code examples and the current section in the side
// navigation. Without JavaScript the page stays readable, just plain.
(() => {
  const keywords = {
    js: "async await break case catch class const continue default else export extends finally for function if import in instanceof let new null of return switch this throw true false try typeof undefined var while",
    ts: "async await break case catch class const continue default else export extends finally for function if import in instanceof interface let new null of return switch this throw true false try type typeof undefined var while string number boolean Promise Record",
    csharp: "async await bool break byte case catch class const default else false finally for foreach if in is new null or and not out private public readonly return static string throw true try using var void while Task",
    rust: "as break const continue crate else enum false fn for if impl in let loop match mod move mut pub ref return self Self static struct trait true type use where while Ok Err Some None Result Option Box Vec String",
    python: "and as break class continue def elif else except False finally for from global if import in is lambda None not or pass raise return True try while with",
  };
  const comment = { js: "//[^\\n]*", ts: "//[^\\n]*", csharp: "//[^\\n]*", rust: "//[^\\n]*", python: "#[^\\n]*" };
  const strings = { js: "`(?:\\\\.|[^`])*`|", ts: "`(?:\\\\.|[^`])*`|", python: "'(?:\\\\.|[^'\\n])*'|", json: "" };

  function highlight(code, lang) {
    if (!keywords[lang] && lang !== "json") return;
    const words = keywords[lang] ? `|\\b(?:${keywords[lang].split(" ").join("|")})\\b` : "";
    const pattern = new RegExp(`(${comment[lang] ?? "(?!)"})|(${strings[lang] ?? ""}"(?:\\\\.|[^"\\\\\\n])*")|(\\b\\d+(?:\\.\\d+)?\\b)${words ? `|(${words.slice(1)})` : ""}`, "g");
    const text = code.textContent;
    const out = document.createDocumentFragment();
    let last = 0;
    for (const m of text.matchAll(pattern)) {
      out.append(text.slice(last, m.index));
      const span = document.createElement("span");
      span.className = m[1] ? "tok-comment" : m[2] ? "tok-string" : m[3] ? "tok-number" : "tok-keyword";
      span.textContent = m[0];
      out.append(span);
      last = m.index + m[0].length;
    }
    out.append(text.slice(last));
    code.replaceChildren(out);
  }

  for (const code of document.querySelectorAll("pre code[class^='language-']")) {
    highlight(code, code.className.slice("language-".length));
  }

  const links = new Map([...document.querySelectorAll(".toc a")].map((a) => [a.hash.slice(1), a]));
  const headings = [...links.keys()].map((id) => document.getElementById(id)).filter(Boolean);
  function mark() {
    // The current section is the last heading above the upper quarter of the window.
    const line = window.innerHeight / 4;
    const current = headings.filter((h) => h.getBoundingClientRect().top <= line).pop() ?? headings[0];
    for (const [id, a] of links) {
      if (current && id === current.id) a.setAttribute("aria-current", "true");
      else a.removeAttribute("aria-current");
    }
  }
  // The page scrolls inside <main>, not the window.
  document.querySelector("main")?.addEventListener("scroll", mark, { passive: true });
  mark();
})();
