import { BLENDS, clamp, defaultOutput, type EditorState, type GradientStop, type Layer, type ProjectMeta, type Stroke } from './state';
import { BrushMask } from './brush';
import { soloVisible } from './solo';
import { previewExposure } from './preview';
import { projectUrl } from '../api/client';
import { useShell } from '../api/shell';
import { perfStart } from './perf';
import { orderedPrefetch } from './prefetch';
import { previewPixels, previewPlan } from './memory';
import { curveEnabled, curveLut } from './curve';
import { imageToScreen, outputToImage, screenToOutput, viewFrame, cropRotation } from './view';

export const logPosition = (nm?: number | null) => clamp(Math.log((nm || 550) / 200) / Math.log(125));
export const wavelengthAt = (t: number) => 200 * Math.pow(125, t);
export const waveLabel = (nm: number) => nm >= 1000 ? `${+(nm / 1000).toFixed(2)} µm` : `${Math.round(nm)} nm`;
export const rgb = (hex: string) => [1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16) / 255);
export const gradientCSS = (stops: GradientStop[]) =>
  `linear-gradient(90deg,${[...stops].sort((a, b) => a.t - b.t).map(s => `${s.color} ${s.t * 100}%`).join(',')})`;
export function wavelengthColor(value?: number | null) {
  const nm = value || 550;
  const stops: [number, number[]][] = [
    [200, [121, 99, 170]], [400, [107, 118, 212]], [490, [91, 162, 199]], [550, [125, 178, 124]],
    [640, [213, 171, 104]], [750, [195, 111, 86]], [2500, [142, 68, 59]], [25000, [102, 67, 51]],
  ];
  const i = Math.max(1, stops.findIndex(s => s[0] >= nm));
  const a = nm > 25000 ? stops[stops.length - 1] : stops[i - 1];
  const b = nm > 25000 ? a : stops[i];
  const t = clamp((Math.log(nm) - Math.log(a[0])) / (Math.log(b[0]) - Math.log(a[0]) || 1));
  return '#' + a[1].map((v, j) => Math.round(v + (b[1][j] - v) * t).toString(16).padStart(2, '0')).join('');
}

const vertex = `#version 300 es
precision highp float;
out vec2 uv;
void main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);uv=p;gl_Position=vec4(p*2.-1.,0.,1.);}`;
const fragment = `#version 300 es
#define FILTER_COUNT 1
precision highp float;
precision highp int;
precision highp sampler2DArray;
precision highp usampler2D;
in vec2 uv;out vec4 result;
uniform sampler2D previous;
uniform sampler2DArray fullImage;
uniform sampler2DArray edgeImage;
uniform sampler2D brushImage;
uniform vec2 edgeAmount;
uniform sampler2D colorMap;
uniform int count;
uniform bool hasLuminance;
uniform int source;
uniform int mode;
uniform int blend;
uniform int stretchMode;
uniform bool invert;
uniform bool hasCurve;
uniform sampler2D curveImage;
uniform sampler2D detailImage;
uniform float detailAmount;
uniform bool tonalPass;
uniform vec4 clipRect;
uniform vec2 clipOptions;
uniform int maskSource;
uniform bool maskInvert;
uniform vec2 maskRange;
uniform vec2 levels;
uniform vec3 tint;
uniform float k;
uniform float intensity;
uniform vec2 toneMatch;
uniform float opacity;
uniform vec4 transform;
uniform float rotation;
uniform float logWave[FILTER_COUNT];
uniform vec2 imageSize;
bool outside(vec2 p){return any(lessThan(p,vec2(0.)))||any(greaterThan(p,vec2(1.)));}
float sampleAt(int i,vec2 p){
 if(outside(p))return 0.;
 return texture(fullImage,vec3(p,float(i))).r;
}
float valueAt(int i,vec2 p){
 if(i>=0)return sampleAt(i,p);
 if(hasLuminance)return sampleAt(count,p);
 float v=0.;for(int j=0;j<FILTER_COUNT;j++){if(j>=count)break;v+=sampleAt(j,p);}return v/max(float(count),1.);
}
float stretchBase(float v){
 float x=max(0.,(v-levels.x)/max(levels.y-levels.x,.00001));
 if(stretchMode==1)x=log(k*x+sqrt(k*k*x*x+1.))/log(k+sqrt(k*k+1.));
 else if(stretchMode==2)x=log(1.+k*x)/log(1.+k);
 else if(stretchMode==3)x=pow(x,1./max(k,.01));
 return x;
}
float detailAt(vec2 q){
 vec2 sz=vec2(textureSize(detailImage,0)),pos=clamp(q*sz-.5,vec2(0.),sz-1.);
 ivec2 lo=ivec2(floor(pos)),hi=min(lo+1,ivec2(sz)-1);vec2 f=fract(pos);
 return mix(mix(texelFetch(detailImage,lo,0).r,texelFetch(detailImage,ivec2(hi.x,lo.y),0).r,f.x),
  mix(texelFetch(detailImage,ivec2(lo.x,hi.y),0).r,texelFetch(detailImage,hi,0).r,f.x),f.y);
}
float stretch(float v){
 float x=stretchBase(v);
 // Post-stretch contrast reveals faint tendrils without letting bright linear-flux peaks dominate.
 if(detailAmount>0.){x=clamp(x,0.,1.);x=clamp(x+detailAmount*(x-detailAt(uv)),0.,1.);}
 if(invert)x=1.-clamp(x,0.,1.);
 if(hasCurve){
  float pos=clamp(x,0.,1.)*255.;int lo=int(floor(pos));
  x=mix(texelFetch(curveImage,ivec2(lo,0),0).r,texelFetch(curveImage,ivec2(min(lo+1,255),0),0).r,fract(pos));
 }
 return clamp(clamp(x*intensity,0.,1.)*toneMatch.x+toneMatch.y,0.,1.);
}
vec3 blendColor(vec3 a,vec3 b){
 if(blend==1)return a+b;
 if(blend==2)return 1.-(1.-a)*(1.-b);
 if(blend==3)return a*b;
 if(blend==4)return mix(2.*a*b,1.-2.*(1.-a)*(1.-b),step(vec3(.5),a));
 if(blend==5)return mix(a-(1.-2.*b)*a*(1.-a),a+(2.*b-1.)*(sqrt(max(a,vec3(0.)))-a),step(vec3(.5),b));
 if(blend==6)return max(a,b);
 if(blend==7)return abs(a-b);
 if(blend==8)return max(a-b,vec3(0.));
 if(blend==9)return min(a,b);
 return b;
}
float edgeHash(ivec2 cell){
 uint h=uint(cell.x)*374761393u+uint(cell.y)*668265263u;
 h=(h^(h>>13u))*1274126177u;h^=h>>16u;
 return float(h&65535u)/65535.;
}
float edgeValueNoise(vec2 p){
 ivec2 cell=ivec2(floor(p));vec2 f=fract(p);f=f*f*(3.-2.*f);
 float a=edgeHash(cell),b=edgeHash(cell+ivec2(1,0)),c=edgeHash(cell+ivec2(0,1)),d=edgeHash(cell+ivec2(1,1));
 return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
}
float edgeNoise(vec2 p){
 return (edgeValueNoise(p)*4.+edgeValueNoise(p*2.)*2.+edgeValueNoise(p*4.))/7.;
}
float clipAlpha(vec2 p){
 if(clipOptions.y==0.)return 1.;
 vec2 size=clipRect.zw*imageSize,q=(p-clipRect.xy)/max(clipRect.zw,vec2(.000001));
 float shortSide=min(size.x,size.y);
 float dist=clipOptions.y==2.?(1.-length((q-.5)*2.))*shortSide*.5:
  min(min(q.x*size.x,(1.-q.x)*size.x),min(q.y*size.y,(1.-q.y)*size.y));
 return clipOptions.x>0.?smoothstep(0.,clipOptions.x*shortSide,dist):step(0.,dist);
}
void main(){
 vec2 p=vec2(uv.x,1.-uv.y)-.5-transform.xy;
 float c=cos(rotation),s=sin(rotation);
 p=(mat2(c,-s,s,c)*(p*imageSize))/imageSize/max(transform.z,.001)+.5;
 vec3 a=texture(previous,uv).rgb;
 if(outside(p)){result=tonalPass?vec4(0.):vec4(a,1.);return;}
 vec3 brush=texture(brushImage,p).rgb;
 if(tonalPass){result=vec4(clamp(stretchBase(valueAt(mode==2?-1:source,p)*brush.g+brush.b),0.,1.),0.,0.,1.);return;}
 float val=valueAt(source,p)*brush.g+brush.b,v=stretch(val);
 vec3 col=tint*v,hue=tint;float bright=v;
 if(mode==1){hue=texture(colorMap,vec2(v,.5)).rgb;col=hue*intensity;}
 if(mode==2){
  float sum=0.,wave=0.,lum=0.;
  for(int i=0;i<FILTER_COUNT;i++){if(i>=count)break;float w=max(0.,sampleAt(i,p));sum+=w;wave+=w*logWave[i];lum+=w;}
  float t=sum>.000001?wave/sum:.5;
  hue=texture(colorMap,vec2(clamp(t,0.,1.),.5)).rgb;
  bright=stretch((hasLuminance?valueAt(-1,p):lum/max(float(count),1.))*brush.g+brush.b);
  col=hue*bright;
 }
 float alpha=opacity;
 // Layer crops are anchored in canvas space, not in the transformed source footprint.
 alpha*=clipAlpha(vec2(uv.x,1.-uv.y));
 // Inverting empty source pixels must not invent coverage outside the band's footprint.
 if(invert||hasCurve)alpha*=max(1.-brush.g,smoothstep(0.,1.,texture(edgeImage,vec3(p,float(source>=0?source:count))).r));
 alpha*=brush.r;
 // Normal: the layer's own brightness is its coverage, so faint areas let lower layers show through.
 if(blend==0){col=hue;alpha*=bright;}
 if(maskSource>=0){float m=clamp((valueAt(maskSource,p)-maskRange.x)/max(.00001,maskRange.y-maskRange.x),0.,1.);alpha*=maskInvert?1.-m:m;}
 if(edgeAmount.x>0.){
  float dist=texture(edgeImage,vec3(p,float(source>=0?source:count))).r;
  float n=edgeNoise(p*imageSize/max(edgeAmount.x,1.));
  float effective=edgeAmount.x*(1.+edgeAmount.y*(n-.5)*1.6);
  alpha*=max(1.-brush.g,smoothstep(0.,effective,dist));
 }
 result=vec4(mix(a,blendColor(a,col),alpha),1.);
}`;
const detailBlurFragment = `#version 300 es
precision highp float;precision highp int;
in vec2 uv;out vec4 result;
uniform sampler2D image;uniform vec2 direction;uniform float sigma;
void main(){
 ivec2 sz=textureSize(image,0),p=ivec2(gl_FragCoord.xy),axis=ivec2(direction);
 int radius=int(floor(4.*sigma+.5));float sum=0.,total=0.;
 for(int i=-radius;i<=radius;i++){
  float weight=exp(-.5*float(i*i)/(sigma*sigma));
  sum+=texelFetch(image,clamp(p+axis*i,ivec2(0),sz-1),0).r*weight;total+=weight;
 }
 result=vec4(sum/total,0.,0.,1.);
}`;
const finalFragment = `#version 300 es
precision highp float;
in vec2 uv;out vec4 result;
uniform sampler2D image;
uniform vec2 viewport;
uniform vec2 imageSize;
uniform vec4 crop;
uniform vec4 outputFrame;
uniform float cropDelta;
uniform vec2 pan;
uniform float zoom;
uniform float rotation;
uniform bool flipX;
uniform float exposure;
uniform float highlights;
uniform float outputBlack;
uniform float outputWhite;
uniform float midtones;
uniform sampler2D brushImage;
uniform bool showBrush;
uniform vec4 brushTransform;
uniform float brushRotation;
uniform vec3 brushTint;
uniform bool previewLinear;
void main(){
 vec2 size=imageSize*outputFrame.zw;
 float fit=min(viewport.x/size.x,viewport.y/size.y);
 vec2 p=(vec2(uv.x,1.-uv.y)*viewport-viewport*.5-pan)/(size*fit*zoom);
 p+=.5;
 if(any(lessThan(p,vec2(0.)))||any(greaterThan(p,vec2(1.)))){result=previewLinear?vec4(0.,0.,0.,1.):vec4(.0784,.0824,.0902,1.);return;}
 p=outputFrame.xy+p*outputFrame.zw-.5;
 if(cropDelta!=0.){
  float dc=cos(cropDelta),ds=sin(cropDelta);
  vec2 q=(mat2(dc,-ds,ds,dc)*(p*imageSize))/imageSize+.5;
  if(any(lessThan(q,crop.xy-vec2(1e-6)))||any(greaterThan(q,crop.xy+crop.zw+vec2(1e-6)))){
   result=previewLinear?vec4(0.,0.,0.,1.):vec4(.0784,.0824,.0902,1.);return;
  }
 }
 float c=cos(rotation),s=sin(rotation);
 p=(mat2(c,-s,s,c)*(p*imageSize))/imageSize;
 if(flipX)p.x=-p.x;
 p+=.5;
 if(any(lessThan(p,vec2(0.)))||any(greaterThan(p,vec2(1.)))){result=previewLinear?vec4(0.,0.,0.,1.):vec4(.0784,.0824,.0902,1.);return;}
 vec3 col=max(texture(image,vec2(p.x,1.-p.y)).rgb,0.)*exp2(exposure);
 if(highlights>0.){float t=1.-.85*highlights,m=max(col.r,max(col.g,col.b));if(m>t){col*=(t+(1.-t)*(1.-exp(-(m-t)/(1.-t))))/m;}}
 if(outputBlack!=0.||outputWhite!=1.||midtones!=1.){
  col=max((col-outputBlack)/max(outputWhite-outputBlack,1e-5),0.);
  if(midtones!=1.)col=pow(col,vec3(midtones));
 }
 if(!previewLinear)col=clamp(col,0.,1.);
 if(showBrush){
  vec2 q=(p-.5-brushTransform.xy)*imageSize;
  float bc=cos(brushRotation),bs=sin(brushRotation);
  q=(mat2(bc,-bs,bs,bc)*q)/imageSize/max(brushTransform.z,.001)+.5;
  if(all(greaterThanEqual(q,vec2(0.)))&&all(lessThanEqual(q,vec2(1.)))){
   vec2 edits=texture(brushImage,q).rg;
   col=mix(col,brushTint,max(1.-edits.r,1.-edits.g)*.42);
  }
 }
 result=vec4(col,1.);
}`;

function program(gl: WebGL2RenderingContext, fragmentSource: string): WebGLProgram {
  const p = gl.createProgram();
  if (!p) throw Error('Cannot create GPU program.');
  for (const [type, text] of [[gl.VERTEX_SHADER, vertex], [gl.FRAGMENT_SHADER, fragmentSource]] as const) {
    const shader = gl.createShader(type);
    if (!shader) throw Error('Cannot create GPU shader.');
    gl.shaderSource(shader, text); gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      const message = gl.getShaderInfoLog(shader);
      gl.deleteShader(shader); gl.deleteProgram(p);
      throw Error(message || 'Shader compilation failed.');
    }
    gl.attachShader(p, shader); gl.deleteShader(shader);
  }
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
    const message = gl.getProgramInfoLog(p); gl.deleteProgram(p);
    throw Error(message || 'Shader linking failed.');
  }
  return p;
}
export function gradientBytes(stops: GradientStop[]) {
  const sorted = [...stops].sort((a, b) => a.t - b.t), bytes = new Uint8Array(256 * 4);
  for (let x = 0; x < 256; x++) {
    const t = x / 255;
    let a = sorted[0], b = sorted[sorted.length - 1];
    if (t <= a.t) b = a;
    else if (t >= b.t) a = b;
    else for (let j = 1; j < sorted.length; j++) if (sorted[j].t >= t) { a = sorted[j - 1]; b = sorted[j]; break; }
    const f = clamp((t - a.t) / (b.t - a.t || 1)), ca = rgb(a.color), cb = rgb(b.color);
    for (let j = 0; j < 3; j++) bytes[x * 4 + j] = Math.round((ca[j] + (cb[j] - ca[j]) * f) * 255);
    bytes[x * 4 + 3] = 255;
  }
  return bytes;
}
type UniformKind = 'uniform1i' | 'uniform1f' | 'uniform2f' | 'uniform3f' | 'uniform4f' | 'uniform1fv';
interface RenderTarget { texture: WebGLTexture; fbo: WebGLFramebuffer; width: number; height: number }
export class Compositor {
  readonly canvas: HTMLCanvasElement;
  readonly gl: WebGL2RenderingContext;
  readonly float: boolean;
  view = { zoom: 1, x: 0, y: 0 };
  ready = false;
  meta!: ProjectMeta;
  state!: EditorState;
  private layerProgram: WebGLProgram;
  private finalProgram: WebGLProgram;
  private locations = new Map<WebGLProgram, Map<string, WebGLUniformLocation | null>>();
  private textures: WebGLTexture[] = [];
  private fbos: WebGLFramebuffer[] = [];
  private vao: WebGLVertexArrayObject | null;
  private lut: WebGLTexture;
  private curveTexture: WebGLTexture;
  private detailProgram: WebGLProgram;
  private detailScratch: RenderTarget | null = null;
  private detailCache = new Map<string, { key: string; target: RenderTarget }>();
  private brushIdentity: WebGLTexture;
  private full!: WebGLTexture;
  private edge!: WebGLTexture;
  private buffers: WebGLTexture[] = [];
  private count = 0;
  private hasLuminance = false;
  private current = 0;
  previewScale = 1;
  private textureWidth = 1;
  private textureHeight = 1;
  previewBrush = new Map<string, Stroke>();
  solo: string[] | null = null;
  showBrush: string | null = null;
  brushTint = [1, .7, .3];
  private brushes = new Map<string, { mask: BrushMask; texture: WebGLTexture }>();
  private previewTargets = new Map<string, RenderTarget>();
  private previewScratch: RenderTarget[] = [];
  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    const g = canvas.getContext('webgl2', { alpha: false, preserveDrawingBuffer: true, antialias: false });
    if (!g) throw Error('WebGL2 is unavailable. Enable hardware acceleration.');
    this.gl = g; this.float = !!g.getExtension('EXT_color_buffer_float');
    g.getExtension('OES_texture_float_linear');
    this.layerProgram = program(g, fragment); this.finalProgram = program(g, finalFragment);
    this.detailProgram = program(g, detailBlurFragment);
    this.vao = g.createVertexArray(); g.bindVertexArray(this.vao);
    this.lut = this.texture2d(256, 1, g.RGBA8, g.RGBA, g.UNSIGNED_BYTE, null);
    this.curveTexture = this.texture2d(256, 1, g.R32F, g.RED, g.FLOAT, curveLut(), false);
    this.brushIdentity = this.texture2d(1, 1, g.RGBA16F, g.RGBA, g.FLOAT, new Float32Array([1, 1, 0, 0]));
  }
  private uniform(p: WebGLProgram, name: string, type: UniformKind, ...values: (number | Float32Array)[]) {
    let locations = this.locations.get(p);
    if (!locations) { locations = new Map(); this.locations.set(p, locations); }
    if (!locations.has(name)) locations.set(name, this.gl.getUniformLocation(p, name));
    const location = locations.get(name) ?? null, g = this.gl;
    const n = (i: number) => values[i] as number;
    switch (type) {
      case 'uniform1i': g.uniform1i(location, n(0)); break;
      case 'uniform1f': g.uniform1f(location, n(0)); break;
      case 'uniform2f': g.uniform2f(location, n(0), n(1)); break;
      case 'uniform3f': g.uniform3f(location, n(0), n(1), n(2)); break;
      case 'uniform4f': g.uniform4f(location, n(0), n(1), n(2), n(3)); break;
      case 'uniform1fv': g.uniform1fv(location, values[0] as Float32Array); break;
    }
  }
  private texture2d(w: number, h: number, internal: number, format: number, type: number,
    data: ArrayBufferView | null, linear = true): WebGLTexture {
    const g = this.gl, t = g.createTexture();
    if (!t) throw Error('Cannot create GPU texture.');
    g.bindTexture(g.TEXTURE_2D, t);
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MIN_FILTER, linear ? g.LINEAR : g.NEAREST);
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MAG_FILTER, linear ? g.LINEAR : g.NEAREST);
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_WRAP_S, g.CLAMP_TO_EDGE);
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_WRAP_T, g.CLAMP_TO_EDGE);
    g.pixelStorei(g.UNPACK_ALIGNMENT, 1);
    g.texImage2D(g.TEXTURE_2D, 0, internal, w, h, 0, format, type, data);
    return t;
  }
  async load(meta: ProjectMeta, notify: (message: string) => void = () => {}, signal?: AbortSignal) {
    const endLoad = perfStart('load');
    const sessionId = useShell.getState().project;
    const g = this.gl; this.ready = false; this.meta = meta; this.count = meta.layers.length;
    for (const cached of this.detailCache.values()) this.deleteTarget(cached.target);
    this.detailCache.clear();
    if (this.detailScratch) this.deleteTarget(this.detailScratch);
    this.detailScratch = null;
    this.hasLuminance = !!meta.luminance;
    const plan = previewPlan(meta.width, meta.height, this.count,
      g.getParameter(g.MAX_ARRAY_TEXTURE_LAYERS) as number, g.getParameter(g.MAX_TEXTURE_SIZE) as number);
    this.previewScale = plan.scale; this.textureWidth = plan.width; this.textureHeight = plan.height;
    this.locations.delete(this.layerProgram); g.deleteProgram(this.layerProgram);
    this.layerProgram = program(g, fragment.replace('#define FILTER_COUNT 1', `#define FILTER_COUNT ${this.count}`));
    const { width: w, height: h } = plan;
    const pixels = (data: Uint16Array, channels = 1) => previewPixels(data, meta.width, meta.height, plan.scale, channels);
    for (const t of this.textures) g.deleteTexture(t);
    for (const f of this.fbos) g.deleteFramebuffer(f);
    this.textures = []; this.fbos = [];
    const fetchBytes = async (file: string, fetchSignal = signal) => {
      const end = perfStart('fetch');
      const response = await fetch(projectUrl(`/api/project/${encodeURIComponent(file)}`, sessionId), { signal: fetchSignal });
      if (!response.ok) throw Error(`Cannot load ${file}: ${response.status}`);
      const bytes = await response.arrayBuffer();
      end();
      return bytes;
    };
    const arrayTexture = (depth = this.count + Number(this.hasLuminance)) => {
      const t = g.createTexture();
      if (!t) throw Error('Cannot create GPU texture.');
      g.bindTexture(g.TEXTURE_2D_ARRAY, t);
      g.texStorage3D(g.TEXTURE_2D_ARRAY, 1, g.R16F, w, h, depth);
      g.texParameteri(g.TEXTURE_2D_ARRAY, g.TEXTURE_MIN_FILTER, g.LINEAR);
      g.texParameteri(g.TEXTURE_2D_ARRAY, g.TEXTURE_MAG_FILTER, g.LINEAR);
      g.texParameteri(g.TEXTURE_2D_ARRAY, g.TEXTURE_WRAP_S, g.CLAMP_TO_EDGE);
      g.texParameteri(g.TEXTURE_2D_ARRAY, g.TEXTURE_WRAP_T, g.CLAMP_TO_EDGE);
      this.textures.push(t); return t;
    };
    this.full = arrayTexture();
    this.edge = arrayTexture(this.count + 1);
    const tasks: { file: string; apply: (bytes: ArrayBuffer) => void }[] = [];
    const upload = (file: string, texture: WebGLTexture, index: number, message?: string) => {
      tasks.push({ file, apply: bytes => {
        if (message) notify(message);
        const endDecode = perfStart('decode');
        const data = pixels(new Uint16Array(bytes));
        endDecode();
        signal?.throwIfAborted();
        if (data.length !== w * h) throw Error(`Invalid layer dimensions: ${file}`);
        g.bindTexture(g.TEXTURE_2D_ARRAY, texture);
        const endUpload = perfStart('upload');
        g.texSubImage3D(g.TEXTURE_2D_ARRAY, 0, 0, 0, index, w, h, 1, g.RED, g.HALF_FLOAT, data);
        endUpload();
      } });
    };
    for (let i = 0; i < this.count; i++) {
      const layer = meta.layers[i];
      upload(layer.file, this.full, i, `Loading ${layer.filter} · ${i + 1}/${this.count}`);
      upload(layer.edge_file || `${layer.key}.edge.f16`, this.edge, i);
    }
    upload(meta.luminance?.edge_file || 'luminance.edge.f16', this.edge, this.count);
    if (meta.luminance) {
      upload(meta.luminance.file, this.full, this.count, 'Loading luminance');
    }
    await orderedPrefetch(tasks, (task, fetchSignal) => fetchBytes(task.file, fetchSignal),
      (task, bytes) => task.apply(bytes), window.__astroimpPerf && window.__astroimpPerfSequential ? 1 : 4, signal);
    signal?.throwIfAborted();
    this.buffers = [0, 1].map(() => {
      const t = this.texture2d(w, h, this.float ? g.RGBA16F : g.RGBA8, g.RGBA,
        this.float ? g.HALF_FLOAT : g.UNSIGNED_BYTE, null);
      this.textures.push(t);
      const f = g.createFramebuffer();
      if (!f) throw Error('Cannot create GPU framebuffer.');
      g.bindFramebuffer(g.FRAMEBUFFER, f);
      g.framebufferTexture2D(g.FRAMEBUFFER, g.COLOR_ATTACHMENT0, g.TEXTURE_2D, t, 0);
      this.fbos.push(f);
      if (g.checkFramebufferStatus(g.FRAMEBUFFER) !== g.FRAMEBUFFER_COMPLETE)
        throw Error('GPU framebuffer is incomplete.');
      return t;
    });
    this.ready = true; g.bindFramebuffer(g.FRAMEBUFFER, null);
    endLoad();
  }
  private bind(unit: number, target: number, texture: WebGLTexture) {
    this.gl.activeTexture(this.gl.TEXTURE0 + unit); this.gl.bindTexture(target, texture);
  }
  private brushTexture(layer: Layer) {
    const g = this.gl;
    g.activeTexture(g.TEXTURE7);
    const preview = this.previewBrush.get(layer.id);
    if (!layer.brush.strokes.length && !preview) return this.brushIdentity;
    let cached = this.brushes.get(layer.id);
    if (!cached) {
      const mask = new BrushMask(this.meta.width, this.meta.height);
      cached = { mask, texture: this.texture2d(mask.width, mask.height, g.RGBA16F, g.RGBA, g.FLOAT, mask.data) };
      this.brushes.set(layer.id, cached);
    }
    if (cached.mask.update(layer.brush.strokes, preview)) {
      g.bindTexture(g.TEXTURE_2D, cached.texture);
      g.texSubImage2D(g.TEXTURE_2D, 0, 0, 0, cached.mask.width, cached.mask.height, g.RGBA, g.FLOAT, cached.mask.data);
    }
    return cached.texture;
  }
  resize() {
    const r = this.canvas.getBoundingClientRect(), d = Math.min(devicePixelRatio || 1, 2);
    const w = Math.max(1, Math.round(r.width * d)), h = Math.max(1, Math.round(r.height * d));
    if (this.canvas.width !== w || this.canvas.height !== h) { this.canvas.width = w; this.canvas.height = h; }
  }
  private prepareDetail(layer: Layer): WebGLTexture {
    if (!(layer.detail?.amount > 0)) return this.brushIdentity;
    const g = this.gl, w = this.textureWidth, h = this.textureHeight;
    const preview = this.previewBrush.get(layer.id);
    const paints = (stroke: Stroke) => stroke.mode === 'black' || stroke.mode === 'white';
    const key = JSON.stringify([layer.source, layer.mode === 'spectral', layer.black, layer.white, layer.stretch,
      layer.k, layer.detail.radius, layer.transform, layer.brush.strokes.filter(paints),
      preview && paints(preview) ? preview : null]);
    let cached = this.detailCache.get(layer.id);
    if (cached?.key === key) {
      this.detailCache.delete(layer.id); this.detailCache.set(layer.id, cached);
      return cached.target.texture;
    }
    g.activeTexture(g.TEXTURE12);
    if (!cached) {
      // A fixed three-entry LRU bounds allocations even with arbitrarily many duplicated layers.
      if (this.detailCache.size === 3) {
        const [id, old] = this.detailCache.entries().next().value!;
        this.deleteTarget(old.target); this.detailCache.delete(id);
      }
      cached = { key: '', target: this.scalarTarget(w, h) }; this.detailCache.set(layer.id, cached);
    }
    this.detailScratch ??= this.scalarTarget(w, h);
    this.bind(12, g.TEXTURE_2D, this.brushIdentity);
    g.useProgram(this.layerProgram);
    this.uniform(this.layerProgram, 'tonalPass', 'uniform1i', 1);
    g.bindFramebuffer(g.FRAMEBUFFER, cached.target.fbo); g.viewport(0, 0, w, h);
    g.drawArrays(g.TRIANGLES, 0, 3);
    this.uniform(this.layerProgram, 'tonalPass', 'uniform1i', 0);
    const p = this.detailProgram;
    g.useProgram(p);
    this.uniform(p, 'image', 'uniform1i', 0);
    this.uniform(p, 'sigma', 'uniform1f', Math.max(1, Math.min(64, layer.detail.radius)) / this.previewScale);
    for (const horizontal of [true, false]) {
      this.bind(0, g.TEXTURE_2D, horizontal ? cached.target.texture : this.detailScratch.texture);
      g.bindFramebuffer(g.FRAMEBUFFER, horizontal ? this.detailScratch.fbo : cached.target.fbo);
      this.uniform(p, 'direction', 'uniform2f', horizontal ? 1 : 0, horizontal ? 0 : 1);
      g.drawArrays(g.TRIANGLES, 0, 3);
    }
    cached.key = key;
    return cached.target.texture;
  }
  compose(state: EditorState, fullStack = false) {
    if (!this.ready) return;
    this.state = state;
    const g = this.gl;
    if (window.__astroimpPerf) g.finish();
    const endCompose = perfStart('recompose');
    for (const [id, cached] of this.brushes) if (!state.layers.some(l => l.id === id)) {
      g.deleteTexture(cached.texture); this.brushes.delete(id);
    }
    for (const [id, cached] of this.detailCache) if (!state.layers.some(l => l.id === id && l.detail?.amount > 0)) {
      this.deleteTarget(cached.target); this.detailCache.delete(id);
    }
    if (!this.detailCache.size && !state.layers.some(l => l.detail?.amount > 0) && this.detailScratch) {
      this.deleteTarget(this.detailScratch); this.detailScratch = null;
    }
    for (const [key, target] of this.previewTargets) if (!state.layers.some(l => key.startsWith(`${l.id}:`))) {
      this.deleteTarget(target); this.previewTargets.delete(key);
    }
    this.current = this.composeLayers(state, this.buffers, this.fbos, this.textureWidth, this.textureHeight, fullStack);
    this.present();
    if (window.__astroimpPerf) {
      g.finish();
      g.readPixels(0, 0, 1, 1, g.RGBA, g.UNSIGNED_BYTE, new Uint8Array(4));
    }
    endCompose();
  }
  private composeLayers(state: EditorState, buffers: WebGLTexture[], fbos: WebGLFramebuffer[], width: number, height: number, fullStack: boolean) {
    const g = this.gl, p = this.layerProgram, { width: w, height: h } = this.meta;
    g.bindVertexArray(this.vao); g.useProgram(p); g.viewport(0, 0, width, height);
    this.bind(1, g.TEXTURE_2D_ARRAY, this.full);
    this.bind(6, g.TEXTURE_2D_ARRAY, this.edge);
    for (const [name, unit] of [
      ['previous', 0], ['fullImage', 1], ['colorMap', 5], ['edgeImage', 6],
      ['brushImage', 7],
      ['curveImage', 11],
      ['detailImage', 12],
    ] as const) this.uniform(p, name, 'uniform1i', unit);
    this.uniform(p, 'count', 'uniform1i', this.count);
    this.uniform(p, 'tonalPass', 'uniform1i', 0);
    this.uniform(p, 'logWave', 'uniform1fv', Float32Array.from(this.meta.layers.map(l => logPosition(l.wavelength_nm))));
    this.uniform(p, 'hasLuminance', 'uniform1i', this.hasLuminance ? 1 : 0);
    this.uniform(p, 'imageSize', 'uniform2f', w, h);
    g.bindFramebuffer(g.FRAMEBUFFER, fbos[0]); g.clearColor(0, 0, 0, 1); g.clear(g.COLOR_BUFFER_BIT);
    let current = 0;
    for (const l of [...state.layers].reverse()) {
      if (!soloVisible(l.id, l.visible, fullStack ? null : this.solo)) continue;
      this.bind(7, g.TEXTURE_2D, this.brushTexture(l));
      const next = 1 - current;
      g.bindFramebuffer(g.FRAMEBUFFER, fbos[next]); this.bind(0, g.TEXTURE_2D, buffers[current]);
      const idx = (key: string) => this.meta.layers.findIndex(s => s.key === key);
      this.uniform(p, 'source', 'uniform1i', idx(l.source));
      this.uniform(p, 'mode', 'uniform1i', ['tint', 'gradient', 'spectral'].indexOf(l.mode));
      this.uniform(p, 'blend', 'uniform1i', BLENDS.indexOf(l.blend));
      this.uniform(p, 'stretchMode', 'uniform1i', ['linear', 'asinh', 'log', 'gamma'].indexOf(l.stretch));
      this.uniform(p, 'invert', 'uniform1i', l.invert ? 1 : 0);
      this.uniform(p, 'hasCurve', 'uniform1i', curveEnabled(l.curve) ? 1 : 0);
      this.bind(11, g.TEXTURE_2D, this.curveTexture);
      if (l.curve?.length) g.texSubImage2D(g.TEXTURE_2D, 0, 0, 0, 256, 1, g.RED, g.FLOAT, curveLut(l.curve));
      this.uniform(p, 'levels', 'uniform2f', l.black, l.white);
      this.uniform(p, 'k', 'uniform1f', Math.max(.001, l.k));
      this.uniform(p, 'intensity', 'uniform1f', l.intensity);
      this.uniform(p, 'detailAmount', 'uniform1f', l.detail?.amount ?? 0);
      this.uniform(p, 'toneMatch', 'uniform2f', l.toneMatch?.gain ?? 1, l.toneMatch?.offset ?? 0);
      this.uniform(p, 'opacity', 'uniform1f', l.opacity);
      this.uniform(p, 'clipRect', 'uniform4f', l.clip?.x ?? 0, l.clip?.y ?? 0, l.clip?.w ?? 1, l.clip?.h ?? 1);
      this.uniform(p, 'clipOptions', 'uniform2f', l.clip?.feather ?? 0, l.clip ? l.clip.shape === 'ellipse' ? 2 : 1 : 0);
      this.uniform(p, 'edgeAmount', 'uniform2f', l.edge.feather, l.edge.irregularity);
      this.uniform(p, 'tint', 'uniform3f', ...rgb(l.color));
      this.uniform(p, 'maskSource', 'uniform1i', l.mask.source ? idx(l.mask.source) : -1);
      this.uniform(p, 'maskRange', 'uniform2f', l.mask.black, l.mask.white);
      this.uniform(p, 'maskInvert', 'uniform1i', l.mask.invert ? 1 : 0);
      this.uniform(p, 'transform', 'uniform4f', l.transform.x, l.transform.y, l.transform.scale, 0);
      this.uniform(p, 'rotation', 'uniform1f', l.transform.rotation * Math.PI / 180);
      this.bind(5, g.TEXTURE_2D, this.lut);
      if (l.mode !== 'tint') {
        const stops = l.mode === 'spectral'
          ? l.spectral.stops.map(s => ({ t: logPosition(s.nm), color: s.color })) : l.gradient;
        g.texSubImage2D(g.TEXTURE_2D, 0, 0, 0, 256, 1, g.RGBA, g.UNSIGNED_BYTE, gradientBytes(stops));
      }
      const detail = this.prepareDetail(l);
      g.useProgram(p); g.viewport(0, 0, width, height);
      g.bindFramebuffer(g.FRAMEBUFFER, fbos[next]);
      this.bind(0, g.TEXTURE_2D, buffers[current]); this.bind(5, g.TEXTURE_2D, this.lut);
      this.bind(12, g.TEXTURE_2D, detail);
      g.drawArrays(g.TRIANGLES, 0, 3); current = next;
    }
    return current;
  }
  present(exporting = false, target?: RenderTarget, image = this.buffers[this.current], state = this.state, previewLinear = false) {
    if (!this.ready || !state) return;
    const g = this.gl, p = this.finalProgram, c = state.global.crop;
    const width = target?.width ?? this.canvas.width, height = target?.height ?? this.canvas.height;
    const d = Math.min(devicePixelRatio || 1, 2);
    g.bindFramebuffer(g.FRAMEBUFFER, target?.fbo ?? null); g.viewport(0, 0, width, height);
    g.useProgram(p); this.bind(0, g.TEXTURE_2D, image);
    this.uniform(p, 'image', 'uniform1i', 0);
    this.uniform(p, 'viewport', 'uniform2f', width, height);
    this.uniform(p, 'imageSize', 'uniform2f', this.meta.width, this.meta.height);
    this.uniform(p, 'crop', 'uniform4f', c.x, c.y, c.w, c.h);
    const b = viewFrame(this.meta, state.global);
    this.uniform(p, 'outputFrame', 'uniform4f', b.x, b.y, b.w, b.h);
    this.uniform(p, 'cropDelta', 'uniform1f', (state.global.rotation - cropRotation(state.global)) * Math.PI / 180);
    this.uniform(p, 'pan', 'uniform2f', exporting ? 0 : this.view.x * d, exporting ? 0 : this.view.y * d);
    this.uniform(p, 'zoom', 'uniform1f', exporting ? 1 : this.view.zoom);
    this.uniform(p, 'rotation', 'uniform1f', state.global.rotation * Math.PI / 180);
    this.uniform(p, 'flipX', 'uniform1i', state.global.flipX ? 1 : 0);
    this.uniform(p, 'exposure', 'uniform1f', state.global.output?.exposure || 0);
    this.uniform(p, 'highlights', 'uniform1f', state.global.output?.highlights || 0);
    this.uniform(p, 'outputBlack', 'uniform1f', state.global.output?.black ?? 0);
    this.uniform(p, 'outputWhite', 'uniform1f', state.global.output?.white ?? 1);
    this.uniform(p, 'midtones', 'uniform1f', state.global.output?.midtones ?? 1);
    this.uniform(p, 'previewLinear', 'uniform1i', previewLinear ? 1 : 0);
    const layer = state.layers.find(l => l.id === this.showBrush);
    this.uniform(p, 'showBrush', 'uniform1i', layer && !exporting ? 1 : 0);
    this.uniform(p, 'brushImage', 'uniform1i', 1);
    if (layer && !exporting) {
      this.bind(1, g.TEXTURE_2D, this.brushTexture(layer));
      this.uniform(p, 'brushTransform', 'uniform4f', layer.transform.x, layer.transform.y, layer.transform.scale, 0);
      this.uniform(p, 'brushRotation', 'uniform1f', layer.transform.rotation * Math.PI / 180);
      this.uniform(p, 'brushTint', 'uniform3f', ...this.brushTint);
    }
    g.drawArrays(g.TRIANGLES, 0, 3);
  }
  private deleteTarget(target: RenderTarget) {
    this.gl.deleteTexture(target.texture); this.gl.deleteFramebuffer(target.fbo);
  }
  private target(width: number, height: number, float = false): RenderTarget {
    const g = this.gl, texture = this.texture2d(width, height, float && this.float ? g.RGBA16F : g.RGBA8,
      g.RGBA, float && this.float ? g.HALF_FLOAT : g.UNSIGNED_BYTE, null), fbo = g.createFramebuffer();
    if (!fbo) throw Error('Cannot create preview framebuffer.');
    g.bindFramebuffer(g.FRAMEBUFFER, fbo);
    g.framebufferTexture2D(g.FRAMEBUFFER, g.COLOR_ATTACHMENT0, g.TEXTURE_2D, texture, 0);
    if (g.checkFramebufferStatus(g.FRAMEBUFFER) !== g.FRAMEBUFFER_COMPLETE) {
      g.deleteTexture(texture); g.deleteFramebuffer(fbo); throw Error('Preview framebuffer is incomplete.');
    }
    return { texture, fbo, width, height };
  }
  private scalarTarget(width: number, height: number): RenderTarget {
    const target = this.target(width, height, true), g = this.gl;
    g.bindTexture(g.TEXTURE_2D, target.texture);
    if (this.float) g.texImage2D(g.TEXTURE_2D, 0, g.R32F, width, height, 0, g.RED, g.FLOAT, null);
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MIN_FILTER, g.NEAREST);
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MAG_FILTER, g.NEAREST);
    return target;
  }
  renderLayerPreview(layer: Layer, state: EditorState, longSide: number): { canvas: HTMLCanvasElement; gain: number } {
    const g = this.gl, { width: w, height: h } = this.meta, crop = state.global.crop;
    const size = (width: number, height: number, side: number) => ({
      width: Math.max(1, Math.round(width / Math.max(width, height) * side)),
      height: Math.max(1, Math.round(height / Math.max(width, height) * side)),
    });
    const raw = size(w, h, Math.min(1024, longSide / Math.min(crop.w, crop.h)));
    if (this.previewScratch[0]?.width !== raw.width || this.previewScratch[0]?.height !== raw.height) {
      this.previewScratch.forEach(t => this.deleteTarget(t));
      this.previewScratch = [this.target(raw.width, raw.height, true), this.target(raw.width, raw.height, true)];
    }
    const b = viewFrame(this.meta, state.global);
    const output = size(b.width, b.height, longSide), key = `${layer.id}:${longSide}`;
    let target = this.previewTargets.get(key);
    if (!target || target.width !== output.width || target.height !== output.height) {
      if (target) this.deleteTarget(target);
      target = this.target(output.width, output.height); this.previewTargets.set(key, target);
    }
    const alone: EditorState = { ...state, layers: [{ ...layer, visible: true, opacity: 1, blend: 'add' }],
      global: { ...state.global, output: { ...defaultOutput } } };
    const meterSize = size(b.width, b.height, 128), meterKey = `${layer.id}:meter`;
    let meter = this.previewTargets.get(meterKey);
    if (!meter || meter.width !== meterSize.width || meter.height !== meterSize.height) {
      if (meter) this.deleteTarget(meter);
      meter = this.target(meterSize.width, meterSize.height, true); this.previewTargets.set(meterKey, meter);
    }
    try {
      const current = this.composeLayers(alone, this.previewScratch.map(t => t.texture), this.previewScratch.map(t => t.fbo),
        raw.width, raw.height, true);
      this.present(true, meter, this.previewScratch[current].texture, alone, true);
      const samples = this.float ? new Float32Array(meter.width * meter.height * 4) : new Uint8Array(meter.width * meter.height * 4);
      g.readPixels(0, 0, meter.width, meter.height, g.RGBA, this.float ? g.FLOAT : g.UNSIGNED_BYTE, samples);
      const gain = previewExposure(samples);
      const exposed = { ...alone, global: { ...alone.global, output: { ...defaultOutput, exposure: Math.log2(gain) } } };
      this.present(true, target, this.previewScratch[current].texture, exposed);
      const pixels = new Uint8Array(output.width * output.height * 4);
      g.readPixels(0, 0, output.width, output.height, g.RGBA, g.UNSIGNED_BYTE, pixels);
      const canvas = document.createElement('canvas'); canvas.width = output.width; canvas.height = output.height;
      const context = canvas.getContext('2d')!, image = context.createImageData(output.width, output.height), row = output.width * 4;
      for (let y = 0; y < output.height; y++) image.data.set(pixels.subarray((output.height - y - 1) * row, (output.height - y) * row), y * row);
      context.putImageData(image, 0, 0);
      return { canvas, gain };
    } finally { g.bindFramebuffer(g.FRAMEBUFFER, null); }
  }
  screenToOutput(x: number, y: number) {
    const rect = this.canvas.getBoundingClientRect();
    return screenToOutput(this.meta, this.state.global, { width: rect.width, height: rect.height, ...this.view }, { x, y });
  }
  screenToImage(x: number, y: number) {
    return outputToImage(this.meta, this.state.global, this.screenToOutput(x, y));
  }
  imageToScreen(x: number, y: number) {
    const rect = this.canvas.getBoundingClientRect();
    return imageToScreen(this.meta, this.state.global, { width: rect.width, height: rect.height, ...this.view }, { x, y });
  }
  async exportPNG(): Promise<Blob> {
    const b = viewFrame(this.meta, this.state.global);
    const cw = this.canvas.width, ch = this.canvas.height, g = this.gl;
    const original = this.state;
    try {
      this.compose(original, true);
      this.canvas.width = Math.max(1, Math.round(b.width));
      this.canvas.height = Math.max(1, Math.round(b.height));
      this.present(true);
      const pixels = new Uint8Array(this.canvas.width * this.canvas.height * 4);
      g.readPixels(0, 0, this.canvas.width, this.canvas.height, g.RGBA, g.UNSIGNED_BYTE, pixels);
      const out = document.createElement('canvas');
      out.width = this.canvas.width; out.height = this.canvas.height;
      const ctx = out.getContext('2d');
      if (!ctx) throw Error('Cannot create PNG canvas.');
      const img = ctx.createImageData(out.width, out.height), row = out.width * 4;
      for (let y = 0; y < out.height; y++)
        img.data.set(pixels.subarray((out.height - y - 1) * row, (out.height - y) * row), y * row);
      ctx.putImageData(img, 0, 0);
      return await new Promise<Blob>((resolve, reject) =>
        out.toBlob(b => b ? resolve(b) : reject(Error('PNG encoding failed.')), 'image/png'));
    } finally { this.canvas.width = cw; this.canvas.height = ch; this.compose(original); }
  }
  dispose() {
    this.ready = false;
    const g = this.gl;
    for (const t of [...this.textures, this.lut, this.curveTexture, this.brushIdentity]) g.deleteTexture(t);
    for (const cached of this.brushes.values()) g.deleteTexture(cached.texture);
    this.brushes.clear();
    for (const target of [...this.previewTargets.values(), ...this.previewScratch]) this.deleteTarget(target);
    this.previewTargets.clear(); this.previewScratch = [];
    for (const cached of this.detailCache.values()) this.deleteTarget(cached.target);
    this.detailCache.clear();
    if (this.detailScratch) this.deleteTarget(this.detailScratch);
    for (const f of this.fbos) g.deleteFramebuffer(f);
    for (const p of [this.layerProgram, this.finalProgram, this.detailProgram]) g.deleteProgram(p);
    g.deleteVertexArray(this.vao);
  }
}
