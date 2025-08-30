// dom
const waveOrig = document.getElementById('waveOrig');
const waveTr = document.getElementById('waveTr');
const origTranscript = document.getElementById('origTranscript');
const trTranscript = document.getElementById('trTranscript');
const asrMeta = document.getElementById('asrMeta');
const trMeta = document.getElementById('trMeta');
const pauseBtn = document.getElementById('pauseBtn');

const asrLangSel = document.getElementById('asrLang');
const targetLangSel = document.getElementById('targetLang');
const asrLangLabel = document.getElementById('asrLangLabel');
const langLabel = document.getElementById('langLabel');

// helpers
const LANG_NAMES = {
  auto: 'Auto-detect',
  en: 'English',
  ja: '日本語 (Japanese)',
  fr: 'Français (French)',
  es: 'Español (Spanish)',
  de: 'Deutsch (German)',
  ko: '한국어 (Korean)',
  zh: '中文(简体) (Chinese, Simplified)',
  'zh-TW': '中文(繁體) (Chinese, Traditional)',
};
const nameFor = code => LANG_NAMES[code] || code;

function setupWave(el, barsCount=48){
  el.innerHTML='';
  const bars=[];
  for(let i=0;i<barsCount;i++){
    const b=document.createElement('div');
    b.className='bar';
    b.style.height='6px';
    el.appendChild(b); bars.push(b);
  }
  return (val)=>{
    const h = Math.max(6, Math.min(100, 6 + val*1.2));
    for(let i=0;i<bars.length-1;i++){ bars[i].style.height = bars[i+1].style.height; }
    bars[bars.length-1].style.height = h + 'px';
  };
}

const pushOrig = setupWave(waveOrig);
const pushTr   = setupWave(waveTr);

let es = null;
let running = false;

function connectSSE(){
  if(es) es.close();
  const target = targetLangSel.value;
  langLabel.textContent = nameFor(target);
  es = new EventSource('/stream/translated?to=' + encodeURIComponent(target));
  es.onmessage = (ev)=>{
    try{
      const o = JSON.parse(ev.data);

      // VU frames
      if(o.db !== undefined){
        const level = Math.max(0, Math.min(60, o.db + 60)); 
        pushOrig(level);
        pushTr(level*0.9);
        asrMeta.textContent = `ASR • ${o.voice? 'voice': 'silence'} • ${o.db.toFixed(1)} dB`;
        return;
      }

      // partial
      if(o.partial){
        origTranscript.textContent = o.text || '';
        return;
      }

      // final
      if(o.text){
        origTranscript.textContent = o.text;
        trTranscript.textContent = o.text;
        if(o.audio_url){
          const a = new Audio(o.audio_url);
          a.play().catch(()=>{});
          trMeta.textContent = 'TTS • speaking…';
          a.onended = ()=> trMeta.textContent = 'TTS • idle';
        }
        return;
      }
    }catch(e){}
  };
  es.onerror = ()=>{};
}

async function startEngine(){
  running = true;
  pauseBtn.classList.remove('paused');
  pauseBtn.setAttribute('aria-pressed','false');

  const asrLang = asrLangSel.value || 'auto';
  asrLangLabel.textContent = nameFor(asrLang);
  try{ await fetch('/start?mode=multi&lang=' + encodeURIComponent(asrLang), {method:'POST'}); }catch(_){}
  connectSSE();
}

async function stopEngine(){
  running = false;
  pauseBtn.classList.add('paused');
  pauseBtn.setAttribute('aria-pressed','true');
  if(es){ es.close(); es=null; }
  try{ await fetch('/stop', {method:'POST'}); }catch(_){}
}

pauseBtn.addEventListener('click', ()=>{ running ? stopEngine() : startEngine(); });

// React to language changes
asrLangSel.addEventListener('change', async ()=>{
  if(running){
    await stopEngine();
    await startEngine();
  }else{
    asrLangLabel.textContent = nameFor(asrLangSel.value);
  }
});
targetLangSel.addEventListener('change', ()=>{
  // reconnect SSE to switch translation target
  if(running) connectSSE();
  langLabel.textContent = nameFor(targetLangSel.value);
});

// autostart
startEngine();
