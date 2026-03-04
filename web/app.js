const statusEls = {
  mode: document.getElementById("mode"),
  state: document.getElementById("state"),
  health: document.getElementById("health"),
  temp_cpu_c: document.getElementById("temp_cpu_c"),
  cpu_util_pct: document.getElementById("cpu_util_pct"),
  pkg_power_w: document.getElementById("pkg_power_w"),
  freq_mhz: document.getElementById("freq_mhz"),
  max_freq_khz: document.getElementById("max_freq_khz"),
  no_turbo: document.getElementById("no_turbo"),
  override_expires_ms: document.getElementById("override_expires_ms"),
  target_temp_c: document.getElementById("target_temp_c"),
  last_update_ms: document.getElementById("last_update_ms"),
  message: document.getElementById("message"),
  toast: document.getElementById("toast"),
  service_state: document.getElementById("service-state"),
  service_summary: document.getElementById("service-summary"),
  test_summary: document.getElementById("test-summary"),
  stress_summary: document.getElementById("stress-summary"),
};

const tempCanvas = document.getElementById("trend-temp");
const utilCanvas = document.getElementById("trend-util");
const freqCanvas = document.getElementById("trend-freq");

const history = {
  temp: [],
  util: [],
  freq: [],
  cap: [],
};
const maxPoints = 90;
const tempRange = { min: 20, max: 100 };
const utilRange = { min: 0, max: 100 };
const POLL_INTERVALS_MS = {
  thermal: 1000,
  stress: 2000,
  service: 5000,
};
const MAX_BACKOFF_MS = 30000;
let validationInProgress = false;
let latestStatus = null;
let latestStressStatus = null;
let latestServiceStatus = null;
let stressActionInProgress = false;
let serviceActionInProgress = false;
let stressDefaultsApplied = false;

function fmtNum(value, digits = 1) {
  if (value === null || value === undefined || Number.isNaN(value)) {
    return "--";
  }
  return Number(value).toFixed(digits);
}

function mhzToGhz(value) {
  if (value === null || value === undefined || Number.isNaN(Number(value))) {
    return null;
  }
  return Number(value) / 1000;
}

function khzToGhz(value) {
  if (value === null || value === undefined || Number.isNaN(Number(value))) {
    return null;
  }
  return Number(value) / 1_000_000;
}

function fmtBool(value) {
  if (value === null || value === undefined) {
    return "--";
  }
  return value ? "true" : "false";
}

function fmtTimestamp(ms) {
  if (!ms) {
    return "--";
  }
  const d = new Date(Number(ms));
  return `${d.toLocaleTimeString()} (${ms})`;
}

function fmtOptional(value) {
  return value === null || value === undefined ? "--" : String(value);
}

function setValue(id, text) {
  const el = statusEls[id];
  if (!el) {
    return;
  }
  el.textContent = text;
}

function setBadgeClass(id, value) {
  const el = statusEls[id];
  if (!el) {
    return;
  }
  el.className = `value ${id}-${value}`;
}

function pushHistory(temp, util, freqMhz, capMhz) {
  history.temp.push(temp ?? NaN);
  history.util.push(util ?? NaN);
  history.freq.push(freqMhz ?? NaN);
  history.cap.push(capMhz ?? NaN);

  if (history.temp.length > maxPoints) {
    history.temp.shift();
    history.util.shift();
    history.freq.shift();
    history.cap.shift();
  }
}

function chartY(value, min, max, plotTop, plotBottom) {
  const span = max - min;
  if (span <= 0) {
    return plotBottom;
  }
  return plotBottom - ((value - min) / span) * (plotBottom - plotTop);
}

function drawSeries(ctx, values, color, min, max, plotLeft, plotRight, plotTop, plotBottom) {
  ctx.beginPath();
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;

  let started = false;
  for (let i = 0; i < values.length; i += 1) {
    const v = values[i];
    if (Number.isNaN(v)) {
      continue;
    }

    const x = plotLeft + (i / Math.max(1, values.length - 1)) * (plotRight - plotLeft);
    const y = chartY(v, min, max, plotTop, plotBottom);
    if (!started) {
      ctx.moveTo(x, y);
      started = true;
    } else {
      ctx.lineTo(x, y);
    }
  }

  ctx.stroke();
}

function setLegendValues(status) {
  const tempEl = document.getElementById("legend-temp");
  const utilEl = document.getElementById("legend-util");
  const freqEl = document.getElementById("legend-freq");
  const capEl = document.getElementById("legend-cap");
  const freqGhz = mhzToGhz(status?.freq_mhz);
  const capGhz = khzToGhz(status?.max_freq_khz);
  if (tempEl) {
    tempEl.textContent = `Temp: ${fmtNum(status?.temp_cpu_c)} C`;
  }
  if (utilEl) {
    utilEl.textContent = `CPU: ${fmtNum(status?.cpu_util_pct)} %`;
  }
  if (freqEl) {
    freqEl.textContent = `Freq: ${fmtNum(freqGhz, 2)} GHz`;
  }
  if (capEl) {
    capEl.textContent = `Cap: ${fmtNum(capGhz, 2)} GHz`;
  }
}

function computeFreqRange() {
  const values = [];
  for (const v of history.freq) {
    if (Number.isFinite(v) && !Number.isNaN(v)) {
      values.push(v);
    }
  }
  for (const v of history.cap) {
    if (Number.isFinite(v) && !Number.isNaN(v)) {
      values.push(v);
    }
  }
  if (latestStatus) {
    const latestCap = khzToGhz(latestStatus.max_freq_khz);
    if (latestCap !== null) {
      values.push(latestCap);
    }
  }

  let maxGhz = 0;
  values.forEach((v) => {
    maxGhz = Math.max(maxGhz, v);
  });
  if (maxGhz < 1) {
    maxGhz = 5;
  }
  return {
    min: 0,
    max: Math.max(1, Math.ceil(maxGhz * 2) / 2),
  };
}

function withCanvasContext(canvas, drawFn) {
  if (!canvas) {
    return;
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    return;
  }

  const scale = window.devicePixelRatio || 1;
  const rect = canvas.getBoundingClientRect();
  canvas.width = Math.max(1, Math.floor(rect.width * scale));
  canvas.height = Math.max(1, Math.floor(rect.height * scale));
  ctx.setTransform(scale, 0, 0, scale, 0, 0);
  drawFn(ctx, rect.width, rect.height);
}

function drawGridChart(canvas, opts) {
  withCanvasContext(canvas, (ctx, w, h) => {
    const plotLeft = 52;
    const plotRight = Math.max(plotLeft + 10, w - 12);
    const plotTop = 14;
    const plotBottom = Math.max(plotTop + 10, h - 18);

    ctx.clearRect(0, 0, w, h);
    ctx.fillStyle = "#fbfefd";
    ctx.fillRect(plotLeft, plotTop, plotRight - plotLeft, plotBottom - plotTop);

    ctx.font = "11px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "#6a787d";
    ctx.strokeStyle = "#e0e8e4";
    ctx.lineWidth = 1;

    opts.ticks.forEach((tickValue) => {
      const y = chartY(tickValue, opts.min, opts.max, plotTop, plotBottom);
      ctx.beginPath();
      ctx.moveTo(plotLeft, y);
      ctx.lineTo(plotRight, y);
      ctx.stroke();
      ctx.fillText(opts.tickLabel(tickValue), 4, y + 4);
    });

    ctx.strokeStyle = "#cdd8d4";
    ctx.strokeRect(plotLeft, plotTop, plotRight - plotLeft, plotBottom - plotTop);

    if (Number.isFinite(opts.target)) {
      const targetY = chartY(opts.target, opts.min, opts.max, plotTop, plotBottom);
      ctx.save();
      ctx.setLineDash([5, 3]);
      ctx.strokeStyle = "#9aadb1";
      ctx.beginPath();
      ctx.moveTo(plotLeft, targetY);
      ctx.lineTo(plotRight, targetY);
      ctx.stroke();
      ctx.restore();
      if (opts.targetLabel) {
        ctx.fillStyle = "#64757a";
        ctx.fillText(opts.targetLabel, plotLeft + 6, targetY - 4);
      }
    }

    for (const series of opts.series) {
      if (series.dash && series.dash.length > 0) {
        ctx.save();
        ctx.setLineDash(series.dash);
      }
      drawSeries(
        ctx,
        series.values,
        series.color,
        opts.min,
        opts.max,
        plotLeft,
        plotRight,
        plotTop,
        plotBottom
      );
      if (series.dash && series.dash.length > 0) {
        ctx.restore();
      }
    }

    ctx.fillStyle = "#42565c";
    ctx.fillText("-90s", plotLeft, h - 5);
    ctx.fillText("now", plotRight - 20, h - 5);
  });
}

function drawTrend() {
  drawGridChart(tempCanvas, {
    series: [{ values: history.temp, color: "#d4522d" }],
    min: tempRange.min,
    max: tempRange.max,
    ticks: [20, 40, 60, 80, 100],
    tickLabel: (v) => `${Math.round(v)}C`,
    target:
      latestStatus && Number.isFinite(Number(latestStatus.target_temp_c))
        ? Number(latestStatus.target_temp_c)
        : NaN,
    targetLabel:
      latestStatus && Number.isFinite(Number(latestStatus.target_temp_c))
        ? `target ${fmtNum(latestStatus.target_temp_c)}C`
        : "",
  });

  drawGridChart(utilCanvas, {
    series: [{ values: history.util, color: "#1b7e9e" }],
    min: utilRange.min,
    max: utilRange.max,
    ticks: [0, 25, 50, 75, 100],
    tickLabel: (v) => `${Math.round(v)}%`,
    target: NaN,
    targetLabel: "",
  });

  const freqRange = computeFreqRange();
  drawGridChart(freqCanvas, {
    series: [
      { values: history.freq, color: "#557f37" },
      { values: history.cap, color: "#8a5a2b", dash: [6, 4] },
    ],
    min: freqRange.min,
    max: freqRange.max,
    ticks: [freqRange.min, (freqRange.min + freqRange.max) / 2, freqRange.max],
    tickLabel: (v) => `${fmtNum(v, 2)}GHz`,
    target: NaN,
    targetLabel: "",
  });
}

function showToast(message, isError = false) {
  statusEls.toast.textContent = message;
  statusEls.toast.style.color = isError ? "#b33e28" : "#4f5f66";
}

function applyStatus(data) {
  latestStatus = data;
  setValue("mode", data.mode ?? "--");
  setValue("state", data.state ?? "--");
  setValue("health", data.health ?? "--");
  setValue("temp_cpu_c", `${fmtNum(data.temp_cpu_c)} C`);
  setValue("cpu_util_pct", `${fmtNum(data.cpu_util_pct)} %`);
  setValue("pkg_power_w", `${fmtNum(data.pkg_power_w)} W`);
  setValue("freq_mhz", `${fmtNum(mhzToGhz(data.freq_mhz), 2)} GHz`);
  setValue("max_freq_khz", `${fmtNum(khzToGhz(data.max_freq_khz), 2)} GHz`);
  setValue("no_turbo", fmtBool(data.no_turbo));
  setValue("override_expires_ms", fmtTimestamp(data.override_expires_ms));
  setValue("target_temp_c", data.target_temp_c ? `${fmtNum(data.target_temp_c)} C` : "--");
  setValue("last_update_ms", fmtTimestamp(data.last_update_ms));
  setValue("message", data.message ?? "--");

  setBadgeClass("state", data.state ?? "unknown");
  setBadgeClass("health", data.health ?? "unknown");
  setLegendValues(data);
  const targetInput = document.getElementById("target-temp");
  if (targetInput) {
    if (Number.isFinite(data.target_min_c)) {
      targetInput.min = String(data.target_min_c);
    }
    if (Number.isFinite(data.target_max_c)) {
      targetInput.max = String(data.target_max_c);
    }
  }

  const freqGhz = mhzToGhz(data.freq_mhz);
  const capGhz = khzToGhz(data.max_freq_khz);
  pushHistory(data.temp_cpu_c, data.cpu_util_pct, freqGhz, capGhz);
  drawTrend();
}

function applyStressStatus(data) {
  latestStressStatus = data;
  if (!statusEls.stress_summary) {
    return;
  }

  const running = data.running ? "running" : "idle";
  const pid = fmtOptional(data.pid);
  const load = fmtOptional(data.cpu_load);
  const workers = fmtOptional(data.workers);
  const duration =
    data.running && (data.duration_sec === null || data.duration_sec === undefined)
      ? "keep-running"
      : fmtOptional(data.duration_sec);
  const lastExit = fmtOptional(data.last_exit_code);
  const msg = fmtOptional(data.last_message);
  const limits = `max_workers=${fmtOptional(data.max_workers)} | max_load=${fmtOptional(data.max_cpu_load)}%`;

  statusEls.stress_summary.textContent =
    `Stress status: ${running} | pid=${pid} | workers=${workers} | load=${load}% | duration=${duration} | ${limits} | last_exit=${lastExit} | ${msg}`;
  syncStressControls(data);
}

function applyServiceStatus(data) {
  latestServiceStatus = data;
  const state = data.active_state || (data.active ? "active" : "unknown");
  if (statusEls.service_state) {
    statusEls.service_state.textContent = `Service: ${state}`;
    statusEls.service_state.className = `service-state ${state}`;
  }

  if (statusEls.service_summary) {
    const enabled = fmtOptional(data.enabled_state);
    const sub = fmtOptional(data.sub_state);
    const pid = fmtOptional(data.main_pid);
    const control = data.control_available ? "yes" : "no";
    const viaSudo = data.used_sudo ? "yes" : "no";
    const msg = fmtOptional(data.last_message);
    statusEls.service_summary.textContent =
      `unit=${fmtOptional(data.unit)} | enabled=${enabled} | sub=${sub} | pid=${pid} | control=${control} | sudo=${viaSudo} | ${msg}`;
  }
  syncServiceControls(data);
}

function syncStressControls(data) {
  const toggle = document.getElementById("stress-toggle");
  if (toggle) {
    toggle.textContent = data.running ? "Stop Stress Test" : "Start Stress Test";
    toggle.classList.toggle("danger", data.running);
    toggle.disabled = stressActionInProgress;
  }

  const workersInput = document.getElementById("stress-workers");
  const loadInput = document.getElementById("stress-load");
  const durationInput = document.getElementById("stress-duration");
  const maxWorkers = Number(data.max_workers);
  const maxLoad = Number(data.max_cpu_load);

  if (workersInput && Number.isFinite(maxWorkers) && maxWorkers >= 1) {
    workersInput.max = String(maxWorkers);
    if (!stressDefaultsApplied && workersInput.dataset.userEdited !== "true") {
      workersInput.value = String(maxWorkers);
    }
  }
  if (loadInput && Number.isFinite(maxLoad) && maxLoad >= 1) {
    loadInput.max = String(maxLoad);
    if (!stressDefaultsApplied && loadInput.dataset.userEdited !== "true") {
      loadInput.value = String(maxLoad);
    }
  }
  if (durationInput && !stressDefaultsApplied && durationInput.dataset.userEdited !== "true") {
    durationInput.value = "0";
  }
  if (!stressDefaultsApplied) {
    stressDefaultsApplied = true;
  }
}

function syncServiceControls(data) {
  const toggle = document.getElementById("service-toggle");
  const restart = document.getElementById("service-restart");
  const controlAllowed = Boolean(data.control_available);
  const active = Boolean(data.active);

  if (toggle) {
    toggle.textContent = active ? "Stop Service" : "Start Service";
    toggle.classList.toggle("danger", active);
    toggle.disabled = serviceActionInProgress || !controlAllowed;
  }
  if (restart) {
    restart.disabled = serviceActionInProgress || !controlAllowed;
  }
}

function setTestSummary(text, className = "") {
  if (!statusEls.test_summary) {
    return;
  }
  statusEls.test_summary.textContent = text;
  statusEls.test_summary.className = `test-summary ${className}`.trim();
}

function setValidationButtonsDisabled(disabled) {
  ["test-baseline", "test-stress"].forEach((id) => {
    const button = document.getElementById(id);
    if (button) {
      button.disabled = disabled;
    }
  });
}

function sleepMs(ms) {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

async function getJson(path) {
  const resp = await fetch(path, { cache: "no-store" });
  if (!resp.ok) {
    throw new Error(`status ${resp.status}`);
  }
  return resp.json();
}

async function fetchStatus() {
  try {
    const payload = await getJson("/thermal/status");
    applyStatus(payload);
    showToast("Live updates active");
    return true;
  } catch (err) {
    showToast(`Status fetch failed: ${err.message}`, true);
    return false;
  }
}

async function fetchStressStatus() {
  try {
    const payload = await getJson("/stress/status");
    applyStressStatus(payload);
    return true;
  } catch (err) {
    showToast(`Stress status fetch failed: ${err.message}`, true);
    return false;
  }
}

async function fetchServiceStatus() {
  try {
    const payload = await getJson("/service/status");
    applyServiceStatus(payload);
    return true;
  } catch (err) {
    if (statusEls.service_summary) {
      statusEls.service_summary.textContent = `Service status fetch failed: ${err.message}`;
    }
    return false;
  }
}

async function runBaselineValidation(options = { toast: true }) {
  const service1 = await getJson("/service/status");
  applyServiceStatus(service1);
  if (!service1.active) {
    throw new Error(`service not active (${service1.active_state})`);
  }

  const status1 = await getJson("/thermal/status");
  applyStatus(status1);
  await sleepMs(2000);

  const status2 = await getJson("/thermal/status");
  const service2 = await getJson("/service/status");
  applyStatus(status2);
  applyServiceStatus(service2);

  const t1 = Number(status1.last_update_ms || 0);
  const t2 = Number(status2.last_update_ms || 0);
  if (!(t2 > t1)) {
    throw new Error(`status not updating (${t1} -> ${t2})`);
  }
  if (!service2.active) {
    throw new Error(`service not active after sample (${service2.active_state})`);
  }

  const summary = `PASS baseline | service=${service2.active_state} | temp=${fmtNum(status2.temp_cpu_c)}C | cpu=${fmtNum(status2.cpu_util_pct)}%`;
  setTestSummary(summary, "pass");
  if (options.toast) {
    showToast(summary);
  }
  return { service: service2, status: status2 };
}

async function runStressValidation() {
  const requestedDuration = Number(document.getElementById("stress-duration")?.value || 0);
  const workers = Number(document.getElementById("stress-workers")?.value || 1);
  const cpuLoad = Number(document.getElementById("stress-load")?.value || 100);
  const maxWorkers = Number(latestStressStatus?.max_workers || 512);
  const maxLoad = Number(latestStressStatus?.max_cpu_load || 100);

  let durationSec = requestedDuration;
  if (!Number.isFinite(durationSec) || durationSec < 0 || durationSec > 86_400) {
    throw new Error("invalid stress duration (0..86400)");
  }
  if (durationSec === 0) {
    durationSec = 30;
  }
  if (!Number.isFinite(workers) || workers < 1 || workers > maxWorkers) {
    throw new Error(`invalid worker count (1..${maxWorkers})`);
  }
  if (!Number.isFinite(cpuLoad) || cpuLoad < 1 || cpuLoad > maxLoad) {
    throw new Error(`invalid cpu load (1..${maxLoad})`);
  }

  await runBaselineValidation({ toast: false });
  await postJson("/stress/start", {
    duration_sec: durationSec,
    workers,
    cpu_load: cpuLoad,
  });

  let maxTemp = 0;
  let maxUtil = 0;
  const startedAt = Date.now();
  const maxRunMs = (durationSec + 20) * 1000;

  while (Date.now() - startedAt <= maxRunMs) {
    const [status, stress, service] = await Promise.all([
      getJson("/thermal/status"),
      getJson("/stress/status"),
      getJson("/service/status"),
    ]);
    applyStatus(status);
    applyStressStatus(stress);
    applyServiceStatus(service);

    maxTemp = Math.max(maxTemp, Number(status.temp_cpu_c || 0));
    maxUtil = Math.max(maxUtil, Number(status.cpu_util_pct || 0));

    const elapsedSec = Math.floor((Date.now() - startedAt) / 1000);
    setTestSummary(
      `Running stress validation ${elapsedSec}s | max_temp=${fmtNum(maxTemp)}C | max_cpu=${fmtNum(maxUtil)}%`,
      ""
    );

    if (!service.active) {
      throw new Error(`service became ${service.active_state}`);
    }
    if (!stress.running && elapsedSec >= 2) {
      break;
    }
    await sleepMs(1000);
  }

  const serviceFinal = await getJson("/service/status");
  applyServiceStatus(serviceFinal);
  if (!serviceFinal.active) {
    throw new Error(`service not active after stress (${serviceFinal.active_state})`);
  }
  const summary = `PASS stress | max_temp=${fmtNum(maxTemp)}C | max_cpu=${fmtNum(maxUtil)}%`;
  setTestSummary(summary, "pass");
  showToast(summary);
}

async function postJson(path, payload) {
  const resp = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });

  let body;
  try {
    body = await resp.json();
  } catch (_err) {
    body = { error: "non-json response" };
  }

  if (!resp.ok) {
    const msg = body.error || `request failed: ${resp.status}`;
    throw new Error(msg);
  }

  return body;
}

function bindControls() {
  document.querySelectorAll("button[data-mode]").forEach((button) => {
    button.addEventListener("click", async () => {
      try {
        const mode = button.getAttribute("data-mode");
        await postJson("/thermal/mode", { mode });
        showToast(`Mode set to ${mode}`);
        await fetchStatus();
      } catch (err) {
        showToast(`Mode update failed: ${err.message}`, true);
      }
    });
  });

  const overrideForm = document.getElementById("override-form");
  overrideForm.addEventListener("submit", async (event) => {
    event.preventDefault();

    try {
      const mode = document.getElementById("override-mode").value;
      const ttlSec = Number(document.getElementById("override-ttl").value);
      await postJson("/thermal/override", { mode, ttl_sec: ttlSec });
      showToast(`Override applied: ${mode} for ${ttlSec}s`);
      await fetchStatus();
    } catch (err) {
      showToast(`Override failed: ${err.message}`, true);
    }
  });

  const targetForm = document.getElementById("target-form");
  targetForm.addEventListener("submit", async (event) => {
    event.preventDefault();

    try {
      const targetTemp = Number(document.getElementById("target-temp").value);
      await postJson("/thermal/target", { target_temp_c: targetTemp });
      showToast(`Target updated to ${targetTemp.toFixed(1)} C`);
      await fetchStatus();
    } catch (err) {
      showToast(`Target update failed: ${err.message}`, true);
    }
  });

  const stressForm = document.getElementById("stress-form");
  stressForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (stressActionInProgress) {
      return;
    }
    stressActionInProgress = true;
    if (latestStressStatus) {
      syncStressControls(latestStressStatus);
    }

    try {
      const current = latestStressStatus || (await getJson("/stress/status"));
      if (current.running) {
        await postJson("/stress/stop", {});
        showToast("Stress test stop requested");
      } else {
        const durationSec = Number(document.getElementById("stress-duration").value);
        const workers = Number(document.getElementById("stress-workers").value);
        const cpuLoad = Number(document.getElementById("stress-load").value);
        const maxWorkers = Number(current.max_workers || 512);
        const maxLoad = Number(current.max_cpu_load || 100);
        if (!Number.isFinite(durationSec) || durationSec < 0 || durationSec > 86_400) {
          throw new Error("duration must be in 0..86400");
        }
        if (!Number.isFinite(workers) || workers < 1 || workers > maxWorkers) {
          throw new Error(`workers must be in 1..${maxWorkers}`);
        }
        if (!Number.isFinite(cpuLoad) || cpuLoad < 1 || cpuLoad > maxLoad) {
          throw new Error(`cpu load must be in 1..${maxLoad}`);
        }

        await postJson("/stress/start", {
          duration_sec: Math.floor(durationSec),
          workers: Math.floor(workers),
          cpu_load: Math.floor(cpuLoad),
        });
        if (durationSec === 0) {
          showToast("Stress test started (keep running)");
        } else {
          showToast(`Stress test started (${Math.floor(durationSec)}s)`);
        }
      }
      await fetchStressStatus();
    } catch (err) {
      showToast(`Stress toggle failed: ${err.message}`, true);
    } finally {
      stressActionInProgress = false;
      if (latestStressStatus) {
        syncStressControls(latestStressStatus);
      }
    }
  });

  ["stress-duration", "stress-workers", "stress-load"].forEach((id) => {
    const input = document.getElementById(id);
    if (input) {
      input.addEventListener("input", () => {
        input.dataset.userEdited = "true";
      });
    }
  });

  const serviceToggle = document.getElementById("service-toggle");
  const serviceRestart = document.getElementById("service-restart");

  if (serviceToggle) {
    serviceToggle.addEventListener("click", async () => {
      if (serviceActionInProgress) {
        return;
      }
      serviceActionInProgress = true;
      if (latestServiceStatus) {
        syncServiceControls(latestServiceStatus);
      }
      try {
        const current = latestServiceStatus || (await getJson("/service/status"));
        const action = current.active ? "stop" : "start";
        await postJson(`/service/${action}`, {});
        showToast(`Service ${action} requested`);
        await fetchServiceStatus();
      } catch (err) {
        showToast(`Service toggle failed: ${err.message}`, true);
      } finally {
        serviceActionInProgress = false;
        if (latestServiceStatus) {
          syncServiceControls(latestServiceStatus);
        }
      }
    });
  }

  if (serviceRestart) {
    serviceRestart.addEventListener("click", async () => {
      if (serviceActionInProgress) {
        return;
      }
      serviceActionInProgress = true;
      if (latestServiceStatus) {
        syncServiceControls(latestServiceStatus);
      }
      try {
        await postJson("/service/restart", {});
        showToast("Service restart requested");
        await fetchServiceStatus();
      } catch (err) {
        showToast(`Service restart failed: ${err.message}`, true);
      } finally {
        serviceActionInProgress = false;
        if (latestServiceStatus) {
          syncServiceControls(latestServiceStatus);
        }
      }
    });
  }

  const testBaseline = document.getElementById("test-baseline");
  if (testBaseline) {
    testBaseline.addEventListener("click", async () => {
      if (validationInProgress) {
        showToast("Validation already running", true);
        return;
      }
      validationInProgress = true;
      setValidationButtonsDisabled(true);
      try {
        setTestSummary("Running baseline validation...", "");
        await runBaselineValidation({ toast: true });
      } catch (err) {
        setTestSummary(`FAIL baseline | ${err.message}`, "fail");
        showToast(`Baseline validation failed: ${err.message}`, true);
      } finally {
        validationInProgress = false;
        setValidationButtonsDisabled(false);
      }
    });
  }

  const testStress = document.getElementById("test-stress");
  if (testStress) {
    testStress.addEventListener("click", async () => {
      if (validationInProgress) {
        showToast("Validation already running", true);
        return;
      }
      validationInProgress = true;
      setValidationButtonsDisabled(true);
      try {
        setTestSummary("Starting stress validation...", "");
        await runStressValidation();
      } catch (err) {
        setTestSummary(`FAIL stress | ${err.message}`, "fail");
        showToast(`Stress validation failed: ${err.message}`, true);
      } finally {
        validationInProgress = false;
        setValidationButtonsDisabled(false);
      }
    });
  }
}

bindControls();

function nextPollDelay(success, failureCount, baseDelayMs) {
  if (success) {
    return { delayMs: baseDelayMs, failures: 0 };
  }
  const failures = Math.min(failureCount + 1, 6);
  return {
    delayMs: Math.min(baseDelayMs * 2 ** failures, MAX_BACKOFF_MS),
    failures,
  };
}

function pollStatusLoop(failures = 0) {
  fetchStatus()
    .then((ok) => {
      const next = nextPollDelay(ok, failures, POLL_INTERVALS_MS.thermal);
      window.setTimeout(() => pollStatusLoop(next.failures), next.delayMs);
    })
    .catch(() => {
      const next = nextPollDelay(false, failures, POLL_INTERVALS_MS.thermal);
      window.setTimeout(() => pollStatusLoop(next.failures), next.delayMs);
    });
}

function pollStressLoop(failures = 0) {
  fetchStressStatus()
    .then((ok) => {
      const next = nextPollDelay(ok, failures, POLL_INTERVALS_MS.stress);
      window.setTimeout(() => pollStressLoop(next.failures), next.delayMs);
    })
    .catch(() => {
      const next = nextPollDelay(false, failures, POLL_INTERVALS_MS.stress);
      window.setTimeout(() => pollStressLoop(next.failures), next.delayMs);
    });
}

function pollServiceLoop(failures = 0) {
  fetchServiceStatus()
    .then((ok) => {
      const next = nextPollDelay(ok, failures, POLL_INTERVALS_MS.service);
      window.setTimeout(() => pollServiceLoop(next.failures), next.delayMs);
    })
    .catch(() => {
      const next = nextPollDelay(false, failures, POLL_INTERVALS_MS.service);
      window.setTimeout(() => pollServiceLoop(next.failures), next.delayMs);
    });
}

let resizeTimer;
window.addEventListener("resize", () => {
  if (resizeTimer) {
    window.clearTimeout(resizeTimer);
  }
  resizeTimer = window.setTimeout(() => drawTrend(), 100);
});

pollStatusLoop();
pollStressLoop();
pollServiceLoop();
