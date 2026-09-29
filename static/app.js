// The values must match LANGUAGES in src/main.rs.
const LANGS = [
  ['English','English'],['Spanish','Spanish'],['French','French'],['German','German'],
  ['Italian','Italian'],['Portuguese','Portuguese'],['Russian','Russian'],
  ['Chinese (Simplified)','Chinese Simplified'],['Chinese (Traditional)','Chinese Traditional'],
  ['Japanese','Japanese'],['Korean','Korean'],['Arabic','Arabic'],['Hindi','Hindi'],
  ['Bengali','Bengali'],['Dutch','Dutch'],['Swedish','Swedish'],['Norwegian','Norwegian'],
  ['Danish','Danish'],['Finnish','Finnish'],['Polish','Polish'],['Czech','Czech'],
  ['Turkish','Turkish'],['Greek','Greek'],['Hebrew','Hebrew'],['Romanian','Romanian'],
  ['Hungarian','Hungarian'],['Ukrainian','Ukrainian'],['Thai','Thai'],
  ['Vietnamese','Vietnamese'],['Indonesian','Indonesian'],['Malay','Malay'],
  ['Catalan','Catalan'],['Swahili','Swahili'],
]
// Shorter names for the pickers on phones, so nothing gets cut off.
const SHORT = {
  'Detect language': 'Detect',
  'Chinese (Simplified)': 'Chinese (Simp.)',
  'Chinese (Traditional)': 'Chinese (Trad.)',
}
const MAX_CHARS = 5000

const $ = (id) => document.getElementById(id)
const $from = $('from'), $to = $('to'), $in = $('input'), $out = $('output')
const $status = $('status'), $count = $('count'), $swap = $('swap')
const $clear = $('clear-btn'), $copy = $('copy-btn'), $about = $('about')

const option = (label, value) => { const o = new Option(label, value); o.dataset.full = label; return o }
$from.add(option('Detect language', 'auto'))
LANGS.forEach(([l, v]) => { $from.add(option(l, v)); $to.add(option(l, v)) })

const phone = matchMedia('(max-width: 720px)')
const fitLabels = () => {
  for (const o of [...$from.options, ...$to.options]) {
    o.text = phone.matches ? (SHORT[o.dataset.full] ?? o.dataset.full) : o.dataset.full
  }
}

// Language choice is remembered in this browser only.
const PREFS_KEY = 'tlp'
let prefs = {}
try { prefs = JSON.parse(localStorage.getItem(PREFS_KEY) || '{}') } catch {}
$from.value = prefs.from || 'auto'
$to.value   = prefs.to   || 'Chinese Simplified'
const savePrefs = () => {
  try { localStorage.setItem(PREFS_KEY, JSON.stringify({ from: $from.value, to: $to.value })) } catch {}
}

const cache = new Map()
const CACHE_LIMIT = 200
const keyOf = (text, from, to) => from + '|' + to + '|' + text
const cachePut = (k, v) => {
  cache.set(k, v)
  if (cache.size > CACHE_LIMIT) cache.delete(cache.keys().next().value)
}

let controller = null
let debounce   = null
let lastKey    = ''
let cooldownUntil = 0

const ERROR_MSG = {
  413: 'Text is too long. The limit is 5,000 characters.',
  429: 'Too many requests. Trying again shortly.',
}
const errorFor = (status) => ERROR_MSG[status] ??
  (status >= 500 ? 'The translation service is unavailable. Try again in a moment.' : 'Translation failed. Try again.')

function fit() { markMore($in); markMore($out) }
const markMore = (t) => t.classList.toggle('more', t.scrollTop + t.clientHeight < t.scrollHeight - 1)

function setStatus(kind, message = '') {
  $status.className = 'status' + (kind === 'error' ? ' error' : '')
  if (kind === 'busy') {
    $status.innerHTML = '<span class="dots" aria-hidden="true"><i></i><i></i><i></i></span><span>Translating</span>'
  } else if (kind === 'error') {
    $status.innerHTML = '<svg class="i" aria-hidden="true"><use href="#i-alert"/></svg><span></span>'
    $status.lastChild.textContent = message
  } else {
    $status.textContent = ''
  }
}

function setOutput(text) {
  $out.value = text
  $out.classList.remove('stale')
  $copy.classList.toggle('show', !!text)
  fit()
}

function syncInput() {
  const n = $in.value.length
  $count.textContent = n.toLocaleString('en') + ' / 5,000'
  $count.classList.toggle('near', n >= MAX_CHARS * 0.9 && n < MAX_CHARS)
  $count.classList.toggle('full', n >= MAX_CHARS)
  $clear.classList.toggle('show', n > 0)
  $swap.disabled = $from.value === 'auto'
  $swap.title = $swap.disabled ? 'Pick a source language to swap' : 'Swap languages'
  fit()
}

async function* sseDeltas(body) {
  const reader = body.getReader()
  const dec = new TextDecoder()
  let buf = ''
  while (true) {
    const { done, value } = await reader.read()
    if (done) return
    buf += dec.decode(value, { stream: true })
    const lines = buf.split('\n')
    buf = lines.pop()
    for (const line of lines) {
      if (!line.startsWith('data: ')) continue
      const d = line.slice(6)
      if (d === '[DONE]') return
      try {
        const t = JSON.parse(d)?.choices?.[0]?.delta?.content
        if (t) yield t
      } catch {}
    }
  }
}

async function translate() {
  const text = $in.value.trim()
  if (!text) return
  const key = keyOf(text, $from.value, $to.value)

  const cached = cache.get(key)
  if (cached !== undefined) {
    controller?.abort()
    setOutput(cached)
    setStatus()
    lastKey = key
    return
  }
  if (key === lastKey) return

  const wait = cooldownUntil - Date.now()
  if (wait > 0) {
    setStatus('error', ERROR_MSG[429])
    clearTimeout(debounce)
    debounce = setTimeout(translate, wait + 50)
    return
  }

  controller?.abort()
  const mine = controller = new AbortController()
  // Keep the old translation, dimmed, until the new one starts arriving.
  $out.classList.add('stale')
  setStatus('busy')
  let acc = ''
  try {
    const res = await fetch('/translate', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text, from: $from.value, to: $to.value }),
      signal: mine.signal,
    })
    if (!res.ok) {
      if (res.status === 429) cooldownUntil = Date.now() + 15000
      setOutput('')
      setStatus('error', errorFor(res.status))
      return
    }
    for await (const chunk of sseDeltas(res.body)) {
      acc += chunk
      setOutput(acc)
    }
    if (acc) {
      cachePut(key, acc)
      lastKey = key
    } else {
      setOutput('')
    }
    setStatus()
  } catch (e) {
    if (e.name === 'AbortError') return
    setOutput(acc)
    setStatus('error', 'Translation failed. Check your connection and try again.')
  }
}

function translateNow() {
  clearTimeout(debounce)
  translate()
}

$in.addEventListener('input', () => {
  syncInput()
  clearTimeout(debounce)
  if (!$in.value.trim()) {
    controller?.abort()
    setOutput('')
    setStatus()
    return
  }
  debounce = setTimeout(translate, 500)
})

$in.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); translateNow() }
})
$in.addEventListener('scroll', () => markMore($in))
$out.addEventListener('scroll', () => markMore($out))

$from.addEventListener('change', () => { savePrefs(); syncInput(); translateNow() })
$to.addEventListener('change',   () => { savePrefs(); translateNow() })

$swap.addEventListener('click', () => {
  if ($from.value === 'auto') return
  $swap.classList.toggle('turned')
  ;[$from.value, $to.value] = [$to.value, $from.value]
  const out = $out.value
  setOutput($in.value)
  $in.value = out
  syncInput()
  savePrefs()
  translateNow()
})

$clear.addEventListener('click', () => {
  $in.value = ''
  controller?.abort()
  clearTimeout(debounce)
  setOutput('')
  setStatus()
  syncInput()
  $in.focus()
})

let copiedTimer = null
$copy.addEventListener('click', async () => {
  if (!$out.value) return
  try { await navigator.clipboard.writeText($out.value) } catch { return }
  $copy.classList.add('done')
  $copy.setAttribute('aria-label', 'Copied')
  clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => {
    $copy.classList.remove('done')
    $copy.setAttribute('aria-label', 'Copy translation')
  }, 1600)
})

function openAbout() { $about.showModal() }
$('about-btn').addEventListener('click', openAbout)
$about.addEventListener('click', (e) => { if (e.target === $about) $about.close() })

phone.addEventListener('change', () => { fitLabels(); fit() })
addEventListener('resize', fit)
document.fonts.ready.then(fit)

fitLabels()
setStatus()
syncInput()
