// The result lists (addons, packs): tiles or a list, remembered in this browser, and a message
// when a search cannot reload them. Loaded in the head, so the saved view is set on <html>
// before the page is drawn and survives htmx swaps.
(function () {
  var key = "forge-view";
  function apply(view) {
    view = view === "list" ? "list" : "tiles";
    document.documentElement.setAttribute("data-view", view);
    mark();
  }
  // The pressed state for screen readers; the buttons come again with every htmx swap.
  function mark() {
    var view = document.documentElement.getAttribute("data-view");
    document.querySelectorAll("[data-set-view]").forEach(function (button) {
      button.setAttribute("aria-pressed", String(button.getAttribute("data-set-view") === view));
    });
  }
  document.addEventListener("DOMContentLoaded", mark);
  document.addEventListener("htmx:afterSwap", mark);
  try {
    apply(localStorage.getItem(key));
  } catch (e) {
    apply("tiles");
  }
  document.addEventListener("click", function (event) {
    var button = event.target.closest("[data-set-view]");
    if (!button) return;
    var view = button.getAttribute("data-set-view");
    apply(view);
    try {
      localStorage.setItem(key, view);
    } catch (e) {}
  });

  // Copy buttons next to read-only fields (the pack link). Hidden until this script runs.
  document.addEventListener("DOMContentLoaded", function () {
    document.querySelectorAll("[data-copy]").forEach(function (button) {
      button.hidden = false;
    });
  });
  document.addEventListener("click", function (event) {
    var button = event.target.closest("[data-copy]");
    if (!button) return;
    var id = button.getAttribute("data-copy");
    var field = document.getElementById(id);
    var status = document.querySelector('[data-copied="' + id + '"]');
    function say(text) {
      if (status) status.textContent = text;
    }
    function manual() {
      field.select();
      say("Press Ctrl+C to copy the selected link.");
    }
    if (!navigator.clipboard) return manual();
    navigator.clipboard.writeText(field.value).then(function () {
      say("Link copied.");
    }, manual);
  });

  // "Install with the launcher" opens survivorsforge://. A browser cannot tell whether that worked,
  // but an opened app takes the focus; when it stays here, point out where to get the launcher.
  document.addEventListener("click", function (event) {
    var link = event.target.closest("[data-launcher]");
    var note = document.querySelector("[data-launcher-missing]");
    if (!link || !note) return;
    var left = false;
    function away() {
      left = true;
    }
    window.addEventListener("blur", away, { once: true });
    setTimeout(function () {
      window.removeEventListener("blur", away);
      if (left || document.hidden) return;
      note.setAttribute("role", "status");
      note.className = "notice warn";
      note.firstChild.textContent = "Nothing opened? The launcher is probably not installed yet. ";
    }, 2000);
  });

  // A caption is saved in place: the page keeps its scroll position and the result shows at the field.
  function saveCaption(form) {
    var status = form.parentNode.querySelector("[data-caption-status]");
    status.className = "caption-status";
    status.textContent = "Saving...";
    fetch(form.action, { method: "POST", body: new URLSearchParams(new FormData(form)) })
      .then(function (response) {
        if (response.ok) {
          form.setAttribute("data-saved", form.querySelector('[name="caption"]').value);
          status.textContent = "Saved.";
          return;
        }
        return response.text().then(function (html) {
          var notice = new DOMParser().parseFromString(html, "text/html").querySelector(".notice.bad");
          status.className = "caption-status bad";
          status.textContent = notice ? notice.textContent : "Could not save the caption.";
        });
      })
      .catch(function () {
        status.className = "caption-status bad";
        status.textContent = "Could not save the caption. Check your connection.";
      });
  }
  document.addEventListener("submit", function (event) {
    var form = event.target.closest("form.caption-form");
    if (!form) return;
    event.preventDefault();
    saveCaption(form);
  });

  // Forms that destroy something ask first; without script they act at once.
  document.addEventListener(
    "submit",
    function (event) {
      var form = event.target.closest("form[data-confirm]");
      if (form && !window.confirm(form.getAttribute("data-confirm"))) {
        event.preventDefault();
        event.stopImmediatePropagation();
      }
    },
    true
  );

  // The upload form takes several screenshots but one icon: the file field follows "Use as".
  document.addEventListener("change", function (event) {
    var select = event.target.closest("select[data-single-file]");
    if (!select) return;
    var field = select.form.querySelector('input[type="file"]');
    field.multiple = select.value !== select.getAttribute("data-single-file");
    if (!field.multiple && field.files.length > 1) field.value = "";
  });

  // An icon is a square of the picked image: after picking a file with "Use as" Icon the image
  // shows with a square to drag and size. Its place goes along in pixels of the image (crop_x,
  // crop_y, crop_size); without script the platform takes the centered square.
  function cropState(form) {
    var box = form.querySelector("[data-cropper]");
    var select = form.querySelector("select[data-single-file]");
    var file = form.querySelector('input[type="file"]').files[0];
    if (!box || !select) return;
    var on = select.value === select.getAttribute("data-single-file") && !!file;
    box.hidden = !on;
    box.querySelectorAll('input[type="hidden"]').forEach(function (i) {
      i.disabled = !on;
    });
    if (!on) return;
    var img = box.querySelector("img");
    var key = file.name + ":" + file.size + ":" + file.lastModified;
    if (img.getAttribute("data-file") === key) return;
    img.setAttribute("data-file", key);
    if (img.src) URL.revokeObjectURL(img.src);
    img.onload = function () {
      var side = Math.min(img.naturalWidth, img.naturalHeight);
      box.crop = { x: (img.naturalWidth - side) / 2, y: (img.naturalHeight - side) / 2, size: side };
      box.querySelector("[data-crop-zoom]").value = 100;
      drawCrop(box);
    };
    img.src = URL.createObjectURL(file);
  }
  function drawCrop(box) {
    var img = box.querySelector("img");
    var c = box.crop;
    c.size = Math.min(c.size, img.naturalWidth, img.naturalHeight);
    c.x = Math.max(0, Math.min(c.x, img.naturalWidth - c.size));
    c.y = Math.max(0, Math.min(c.y, img.naturalHeight - c.size));
    var scale = img.clientWidth / img.naturalWidth;
    var square = box.querySelector(".cropper-square");
    square.style.left = c.x * scale + "px";
    square.style.top = c.y * scale + "px";
    square.style.width = square.style.height = c.size * scale + "px";
    box.querySelector('[name="crop_x"]').value = Math.round(c.x);
    box.querySelector('[name="crop_y"]').value = Math.round(c.y);
    box.querySelector('[name="crop_size"]').value = Math.floor(c.size);
  }
  document.addEventListener("change", function (event) {
    if (event.target.form && event.target.form.querySelector("[data-cropper]")) cropState(event.target.form);
  });
  document.addEventListener("input", function (event) {
    if (!event.target.matches("[data-crop-zoom]")) return;
    var box = event.target.closest("[data-cropper]");
    var img = box.querySelector("img");
    var middle = { x: box.crop.x + box.crop.size / 2, y: box.crop.y + box.crop.size / 2 };
    box.crop.size = Math.max(1, (Math.min(img.naturalWidth, img.naturalHeight) * event.target.value) / 100);
    box.crop.x = middle.x - box.crop.size / 2;
    box.crop.y = middle.y - box.crop.size / 2;
    drawCrop(box);
  });
  // Arrow keys move the square by 2 % of the image, with Shift by 10 %.
  document.addEventListener("keydown", function (event) {
    var square = event.target.closest && event.target.closest(".cropper-square");
    var steps = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[event.key];
    if (!square || !steps) return;
    event.preventDefault();
    var box = square.closest("[data-cropper]");
    var img = box.querySelector("img");
    var step = (Math.min(img.naturalWidth, img.naturalHeight) * (event.shiftKey ? 10 : 2)) / 100;
    box.crop.x += steps[0] * step;
    box.crop.y += steps[1] * step;
    drawCrop(box);
  });
  document.addEventListener("pointerdown", function (event) {
    var square = event.target.closest(".cropper-square");
    if (!square) return;
    var box = square.closest("[data-cropper]");
    var img = box.querySelector("img");
    var start = { px: event.clientX, py: event.clientY, x: box.crop.x, y: box.crop.y, scale: img.clientWidth / img.naturalWidth };
    square.setPointerCapture(event.pointerId);
    function move(e) {
      box.crop.x = start.x + (e.clientX - start.px) / start.scale;
      box.crop.y = start.y + (e.clientY - start.py) / start.scale;
      drawCrop(box);
    }
    function stop() {
      square.removeEventListener("pointermove", move);
      square.removeEventListener("pointerup", stop);
    }
    square.addEventListener("pointermove", move);
    square.addEventListener("pointerup", stop);
  });

  // Screenshots in the editor: dragged by their handle, the new order is saved at once. The arrow
  // buttons stay for the keyboard and without script.
  var dragged = null;
  document.addEventListener("mousedown", function (event) {
    var handle = event.target.closest("[data-drag-handle]");
    if (handle) handle.closest("[data-image-id]").draggable = true;
  });
  document.addEventListener("dragstart", function (event) {
    var figure = event.target.closest && event.target.closest("[data-image-id]");
    if (!figure || !figure.draggable) return;
    dragged = figure;
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", figure.getAttribute("data-image-id"));
    figure.classList.add("dragging");
  });
  document.addEventListener("dragover", function (event) {
    if (!dragged) return;
    event.preventDefault();
    var over = event.target.closest("[data-image-id]");
    if (!over || over === dragged || over.parentNode !== dragged.parentNode) return;
    var box = over.getBoundingClientRect();
    over.parentNode.insertBefore(dragged, event.clientX > box.left + box.width / 2 ? over.nextSibling : over);
  });
  document.addEventListener("drop", function (event) {
    if (dragged) event.preventDefault();
  });
  document.addEventListener("dragend", function () {
    if (!dragged) return;
    var figure = dragged;
    dragged = null;
    figure.classList.remove("dragging");
    figure.draggable = false;
    var list = figure.parentNode;
    var ids = Array.prototype.map.call(list.querySelectorAll("[data-image-id]"), function (f) {
      return f.getAttribute("data-image-id");
    });
    var body = new URLSearchParams({ order: ids.join(","), csrf: document.querySelector('input[name="csrf"]').value });
    saved(fetch(list.getAttribute("data-order-url"), { method: "POST", body: body }), list);
  });

  // The arrow buttons move the screenshot in place too, so the page keeps its scroll position.
  document.addEventListener("submit", function (event) {
    var form = event.target.closest("form[data-move]");
    if (!form) return;
    event.preventDefault();
    var figure = form.closest("[data-image-id]");
    var list = figure.parentNode;
    if (new FormData(form).get("direction") === "up") list.insertBefore(figure, figure.previousElementSibling);
    else list.insertBefore(figure, figure.nextElementSibling && figure.nextElementSibling.nextElementSibling);
    saved(fetch(form.action, { method: "POST", body: new URLSearchParams(new FormData(form)) }), list);
  });

  // After a move is saved the first and last arrows switch off; a failed save shows the real order.
  function saved(request, list) {
    request
      .then(function (response) {
        if (!response.ok) throw new Error(response.status);
        var figures = list.querySelectorAll("[data-image-id]");
        figures.forEach(function (f, i) {
          var buttons = f.querySelectorAll("form[data-move] button");
          buttons[0].disabled = i === 0;
          buttons[1].disabled = i === figures.length - 1;
        });
      })
      .catch(function () {
        location.reload();
      });
  }

  // Screenshots on an addon page open in a viewer instead of a new tab: wheel, buttons, + and - or a
  // double click zoom around the pointer, dragging moves the zoomed image, the arrows step through.
  // ponytail: no pinch zoom on touch screens yet; the buttons and a double tap cover it.
  var view = { shots: [], index: 0, scale: 1, x: 0, y: 0, drag: null };
  function viewer() {
    return document.querySelector("dialog.lightbox");
  }
  function place() {
    var img = viewer().querySelector(".lightbox-stage img");
    img.style.transform = "translate(" + view.x + "px, " + view.y + "px) scale(" + view.scale + ")";
    img.style.cursor = view.scale > 1 ? (view.drag ? "grabbing" : "grab") : "zoom-in";
  }
  // Zooms by `factor` around the point (cx, cy), measured from the middle of the stage.
  function zoom(factor, cx, cy) {
    var scale = Math.min(6, Math.max(1, view.scale * factor));
    view.x = cx - ((cx - view.x) * scale) / view.scale;
    view.y = cy - ((cy - view.y) * scale) / view.scale;
    view.scale = scale;
    if (scale === 1) view.x = view.y = 0;
    place();
  }
  function fromMiddle(event) {
    var box = viewer().querySelector(".lightbox-stage").getBoundingClientRect();
    return [event.clientX - box.left - box.width / 2, event.clientY - box.top - box.height / 2];
  }
  function show(index) {
    var box = viewer();
    view.index = (index + view.shots.length) % view.shots.length;
    var link = view.shots[view.index];
    var img = box.querySelector(".lightbox-stage img");
    var caption = link.closest("figure").querySelector("figcaption");
    img.src = link.href;
    img.alt = link.querySelector("img").alt;
    box.querySelector(".lightbox-caption").textContent = caption ? caption.textContent : "";
    box.querySelector(".lightbox-count").textContent = view.shots.length > 1 ? view.index + 1 + " / " + view.shots.length : "";
    box.querySelectorAll("[data-step]").forEach(function (b) {
      b.hidden = view.shots.length < 2;
    });
    view.scale = 1;
    view.x = view.y = 0;
    place();
  }
  document.addEventListener("click", function (event) {
    var link = event.target.closest("[data-lightbox]");
    var box = viewer();
    if (link && box) {
      event.preventDefault();
      view.shots = Array.prototype.slice.call(link.closest(".shots").querySelectorAll("[data-lightbox]"));
      box.showModal();
      show(view.shots.indexOf(link));
      return;
    }
    if (!box || !box.open) return;
    if (event.target === box || event.target.classList.contains("lightbox-stage")) return box.close();
    var step = event.target.closest("[data-step]");
    if (step) return show(view.index + Number(step.getAttribute("data-step")));
    var button = event.target.closest("[data-zoom]");
    if (!button) return;
    var how = button.getAttribute("data-zoom");
    if (how === "fit") zoom(1 / view.scale, 0, 0);
    else zoom(how === "in" ? 1.5 : 1 / 1.5, 0, 0);
  });
  document.addEventListener("keydown", function (event) {
    var box = viewer();
    if (!box || !box.open || event.target.closest("button")) return;
    if (event.key === "ArrowRight") show(view.index + 1);
    else if (event.key === "ArrowLeft") show(view.index - 1);
    else if (event.key === "+" || event.key === "=") zoom(1.5, 0, 0);
    else if (event.key === "-") zoom(1 / 1.5, 0, 0);
    else if (event.key === "0") zoom(1 / view.scale, 0, 0);
  });
  document.addEventListener("DOMContentLoaded", function () {
    var box = viewer();
    if (!box) return;
    var stage = box.querySelector(".lightbox-stage");
    var img = stage.querySelector("img");
    stage.addEventListener(
      "wheel",
      function (event) {
        event.preventDefault();
        var at = fromMiddle(event);
        zoom(event.deltaY < 0 ? 1.2 : 1 / 1.2, at[0], at[1]);
      },
      { passive: false }
    );
    img.addEventListener("dblclick", function (event) {
      var at = fromMiddle(event);
      zoom(view.scale > 1 ? 1 / view.scale : 2.5, at[0], at[1]);
    });
    img.addEventListener("pointerdown", function (event) {
      if (view.scale === 1) return;
      view.drag = { px: event.clientX, py: event.clientY, x: view.x, y: view.y };
      img.setPointerCapture(event.pointerId);
      place();
    });
    img.addEventListener("pointermove", function (event) {
      if (!view.drag) return;
      view.x = view.drag.x + event.clientX - view.drag.px;
      view.y = view.drag.y + event.clientY - view.drag.py;
      place();
    });
    img.addEventListener("pointerup", function () {
      view.drag = null;
      place();
    });
  });

  // htmx keeps the old results when a reload fails; say so above them.
  function failed(event) {
    var target = event.detail && event.detail.target;
    if (!target || target.id !== "results") return;
    var note = target.querySelector(".load-error");
    if (!note) {
      note = document.createElement("div");
      note.className = "notice bad load-error";
      note.setAttribute("role", "alert");
      target.prepend(note);
    }
    note.textContent = "Could not load the results. Check your connection and try again.";
  }
  document.addEventListener("htmx:responseError", failed);
  document.addEventListener("htmx:sendError", failed);
})();
