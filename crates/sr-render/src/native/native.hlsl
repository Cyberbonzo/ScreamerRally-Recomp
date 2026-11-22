// Native renderer: the world from GPU-resident geometry (see native/mod.rs).
//
// Spaces: model -> frame space (world minus the frame origin, the main camera's position;
// per-slot 3x4 transforms computed in f64 on the CPU) -> view space (x right, y down,
// z forward) -> clip space by the game's projection. Depth is reversed and infinite:
// z_ndc = depth_scale / z (larger = nearer), the same as the Glide path's depth buffer
// ((1/w) / 256 with w = the game's view depth = z here), so both share one depth buffer.

cbuffer ViewCb : register(b0)
{
    float4 rot0;        // rows of the world-to-view rotation (xyz)
    float4 rot1;
    float4 rot2;
    float4 offset;      // xyz: frame origin - camera position; w: depth scale
    float4 proj;        // clip_x = x * proj.x + z * proj.y; clip_y = y * proj.z + z * proj.w
    float4 clip;        // x: near, y: far, z: fog on, w: fog w per view-space unit
    float4 fog_color;   // rgb
    float4 fog_table[16];
    uint4  misc;        // x: animation clock, y: env phase, z: V_* flags, w: static fades given
};

#define V_MIRROR  1
#define V_FADES   2
#define V_BACKING 4
#define V_DYN_FADES 8

// Per draw: a decal's depth pull = material pull * draw.x + draw.y (renderer.rs DRAW_*:
// (1, 0) normally; (0, 0) a decal over moving objects; (0, p) moving objects' overlays);
// draw.z > 0: texels with less alpha are discarded (the overlays' depth pass); draw.w > 0:
// only statics of backing pass draw.w - 1 (the backing test's opaque passes)
cbuffer DrawCb : register(b2)
{
    float4 draw;
};

struct Xform
{
    float4 r0;          // model -> frame space, rows: xyz basis, w translation
    float4 r1;
    float4 r2;
    float4 extra;       // dynamic instances: xy reflection scroll, z phase (int bits), w (uint
                        // bits): fade (bits 0-7, with V_DYN_FADES), tested object + 1 (8-31)
    float4 dissolve;    // dynamic instances (FrameData::dynamic_dissolve): x, y the earlier and
                        // later step's phase (int bits), z t, w (uint bits) DISSOLVE_*
};

#define DISSOLVE_ON   1
#define DISSOLVE_FROM 2
#define DISSOLVE_TO   4

struct Mat
{
    uint   flags;
    uint   slice;       // layer in the bound texture array
    float  pull;        // decals: depth as if this much nearer
    uint   anim_first;  // animated: first entry in frame_slices
    uint   anim_count;
    float  stand;       // F_STAND: the ground lies this far under the instance's origin
    uint2  pad;
    float4 env;         // reflection: uv scale (xy), offset (zw)
};

#define F_TEXTURED 1
#define F_KEYED    2
#define F_REFLECT  4
#define F_SKY      8
#define F_REPLACE  16
#define F_ANIM     32
#define F_ANIM_ENV 64
#define F_STAND    128
#define F_NOFOG    256

#define STATIC_BIT 0x80000000u

StructuredBuffer<Xform>  xforms       : register(t0);
StructuredBuffer<Mat>    mats         : register(t1);
Texture2DArray           tex          : register(t2);
StructuredBuffer<uint4>  statics      : register(t3);   // per static: chunk slot, phase, scroll (float bits)
StructuredBuffer<uint>   fades        : register(t4);   // per static: fade, four per uint
StructuredBuffer<float3> normals      : register(t5);   // per vertex (SV_VertexID)
StructuredBuffer<uint>   frame_slices : register(t6);   // animation frames' layers
StructuredBuffer<uint>   backing      : register(t7);   // per static: pass (bits 0-1), tests (2, 3), test slot (8-31)
StructuredBuffer<float>  dyn_tops     : register(t8);   // per tested moving object: its base's top (frame space y)
SamplerState             samp         : register(s0);

// ---- formulas shared with the unit tests (prepare.rs has the CPU references) ----

// The animation frame: min(count - 1, (count * v) >> shift) without 64-bit integers
// (count < 2^16): when v >= 2^shift the product is at least count << shift.
uint anim_frame(uint count, uint v, uint shift)
{
    if ((v >> shift) != 0)
        return count - 1;
    return min(count - 1, (count * v) >> shift);
}

// The game's eye is this far behind its view origin (sr-scene; topic D).
static const float GAME_EYE = 128.0;

// Reflection texture coordinates in texels (the game's mode 5, bb380; sr-scene
// TexGen::Reflection): e = the vertex's view position, n = its normal in view space (both in
// the renderer's view space: x right, y down, z forward, eye at the origin). In the game's
// view space (x left, y up, z back, origin GAME_EYE in front of the eye) the position is
// (-x, -y, 128 - z) (mirror: the game negates x after the transform: (x, -y, 128 - z)) and a
// direction -n. round() is round-half-to-even, like fistp.
float2 reflect_texel(float3 e, float3 n, float2 scroll, bool mirror)
{
    precise float3 p = float3(mirror ? e.x : -e.x, -e.y, GAME_EYE - e.z);
    precise float3 g = -n;
    precise float3 d = p / sqrt(dot(p, p));
    precise float k = 2.0 * dot(d, g);
    precise float3 r = k * g - d;
    return float2(round(127.0 * r.x) + 127.0 - scroll.x, round(127.0 * r.z) + 127.0 - scroll.y);
}

// Screen-door fade: a pixel of a static faded to f (0..255) is drawn when
// 16 f > 255 level + 128, level = the 4x4 ordered-dither threshold of the pixel.
bool fade_draws(uint f, uint2 px)
{
    static const uint bayer[16] = { 0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5 };
    return f * 16 > bayer[(px.y & 3) * 4 + (px.x & 3)] * 255 + 128;
}

// ---- world ----

struct VsIn
{
    float3 pos   : POSITION;
    float2 uv    : TEXCOORD;
    float4 color : COLOR;
    uint   slot  : SLOT;
    uint   mat   : MATERIAL;
    uint   inst  : INSTSLOT;    // per instance: transform slot added to the vertex's
    uint   vid   : SV_VertexID;
};

struct PsIn
{
    float4 pos   : SV_Position;
    float4 color : COLOR;
    float2 uv    : TEXCOORD0;
    float  depth : TEXCOORD1;   // view depth (fog)
    nointerpolation uint mat   : MATERIAL;
    nointerpolation uint slice : SLICE;
    nointerpolation uint fade  : FADE;
    // a dissolve between two steps' pictures: the earlier step's layer, and (t, there at the
    // earlier step, there at the later one, -); t < 0: none
    nointerpolation uint slice0 : PREVSLICE;
    nointerpolation float4 dslv : DISSOLVE;
    float2 cd    : SV_ClipDistance0;    // near, far
};

// A material's animation frame layer for animation value `phase` (negative: the clock).
uint anim_slice(Mat m, int phase)
{
    bool env = (m.flags & F_ANIM_ENV) != 0;
    uint val = env ? misc.y : (phase >= 0 ? (uint)phase : misc.x);
    return frame_slices[m.anim_first + anim_frame(m.anim_count, val, env ? 16 : 14)];
}

PsIn vs_main(VsIn v)
{
    uint s = v.slot + v.inst;
    Xform x;
    int phase;
    float2 scroll;
    uint fade = 255;
    if (s & STATIC_BIT)
    {
        uint si = s & ~STATIC_BIT;
        uint4 st = statics[si];
        x = xforms[st.x];
        phase = (int)st.y;
        scroll = asfloat(st.zw);
        if ((misc.z & V_FADES) && si < misc.w)
            fade = (fades[si >> 2] >> ((si & 3) * 8)) & 255;
        // the backing test's passes: a static is drawn in its own pass only
        if ((misc.z & V_BACKING) && draw.w > 0.5 && (backing[si] & 3u) + 1u != (uint)(draw.w + 0.5))
            fade = 0;
    }
    else
    {
        x = xforms[s];
        phase = asint(x.extra.z);
        scroll = x.extra.xy;
        // moving objects not backed by the world (docs/re/native/visibility.md)
        if (misc.z & V_DYN_FADES)
            fade = asuint(x.extra.w) & 255u;
    }
    Mat m = mats[v.mat];
    float4 p = float4(v.pos, 1.0);
    float3 f;
    if (m.flags & F_SKY)
        f = float3(dot(x.r0.xyz, v.pos), dot(x.r1.xyz, v.pos), dot(x.r2.xyz, v.pos));    // around the camera
    else
        f = float3(dot(x.r0, p), dot(x.r1, p), dot(x.r2, p)) + offset.xyz;
    float3 e = float3(dot(rot0.xyz, f), dot(rot1.xyz, f), dot(rot2.xyz, f));
    float z = e.z;
    // depth after the divide by w = z: offset.w / z, decals tested as if `pull` units nearer
    // (never nearer than a quarter of the depth): offset.w / zd. The clip-space z must stay
    // finite and between 0 and w along every edge in front of the near plane, also from a
    // vertex behind the eye (z <= 0: world polygons beside the camera, docs/re/native/near.md),
    // or the depth clip cuts the triangle off at its front vertices: offset.w for the world
    // (exactly offset.w * z / z of the vertices in front of the eye), and for decals behind
    // the eye the ratio's limit z / zd = 4. The pull is the material's scaled per draw
    // (DrawCb: moving objects' rules, renderer.rs ST_*).
    float pull = m.pull * draw.x + draw.y;
    // a card standing on the ground (Layer::Decal::stand, the puffs), seen from above its
    // base: below the horizontal plane through the base, the depth where the view ray meets
    // that plane (f is relative to the eye, world axes, y up; the ray meets the plane at the
    // fraction g / f.y of the way to the vertex), so the ground it stands on does not cut it
    float zt = z;
    if ((m.flags & F_STAND) && pull > 0.0 && !(s & STATIC_BIT))
    {
        float g = x.r1.w + offset.y - m.stand;
        if (g < 0.0 && f.y < g)
            zt = z * (g / f.y);
    }
    float zd = pull > 0.0 ? max(zt - pull, z * 0.25) : z;
    float zc = pull > 0.0 ? (zd > 0.0 ? offset.w * z / zd : 4.0 * offset.w) : offset.w;
    PsIn o;
    o.pos = float4(e.x * proj.x + z * proj.y, e.y * proj.z + z * proj.w, zc, z);
    o.color = v.color;
    o.uv = v.uv;
    o.depth = z;
    o.mat = v.mat;
    // animated textures: the frame from the instance's phase (or the clock) or the env phase;
    // a dissolve between two steps: the frames of both steps' phases
    uint dbits = (s & STATIC_BIT) ? 0u : asuint(x.dissolve.w);
    o.slice = m.slice;
    o.slice0 = m.slice;
    o.dslv = float4(-1.0, 0.0, 0.0, 0.0);
    if (m.flags & F_ANIM)
    {
        if ((dbits & DISSOLVE_ON) && !(m.flags & F_ANIM_ENV))
        {
            o.slice0 = anim_slice(m, asint(x.dissolve.x));
            o.slice = anim_slice(m, asint(x.dissolve.y));
        }
        else
        {
            o.slice = anim_slice(m, phase);
            o.slice0 = o.slice;
        }
    }
    if (dbits & DISSOLVE_ON)
        o.dslv = float4(saturate(x.dissolve.z), (dbits & DISSOLVE_FROM) ? 1.0 : 0.0, (dbits & DISSOLVE_TO) ? 1.0 : 0.0, 0.0);
    // reflection: per vertex from the normal, as the game
    if (m.flags & F_REFLECT)
    {
        float3 nm = normals[v.vid];
        float3 nf = float3(dot(x.r0.xyz, nm), dot(x.r1.xyz, nm), dot(x.r2.xyz, nm));
        float3 nv = float3(dot(rot0.xyz, nf), dot(rot1.xyz, nf), dot(rot2.xyz, nf));
        o.uv = reflect_texel(e, nv, scroll, (misc.z & V_MIRROR) != 0) * (1.0 / 256.0) * m.env.xy + m.env.zw;
    }
    o.fade = fade;
    // faded out: every corner outside, the triangle is clipped before rasterising
    o.cd = fade == 0 ? float2(-1.0, -1.0) : (m.flags & F_SKY) ? float2(1.0, 1.0) : float2(z - clip.x, clip.y - z);
    return o;
}

float fog_from_w(float w)
{
    // Glide fog table: entry i is at w = 2^(3 + i/4) / (8 - i%4), i.e. 1 .. 52428
    w = clamp(w, 1.0, 52428.0);
    float e = floor(log2(w));
    float mt = w / exp2(e);
    float k = mt >= 1.6 ? 3.0 : mt >= 1.3333 ? 2.0 : mt >= 1.142857 ? 1.0 : 0.0;
    float i = min(e * 4.0 + k, 62.0);
    float wi = exp2(3.0 + floor(i / 4.0)) / (8.0 - fmod(i, 4.0));
    float wj = exp2(3.0 + floor((i + 1.0) / 4.0)) / (8.0 - fmod(i + 1.0, 4.0));
    float t = saturate((w - wi) / (wj - wi));
    uint a = (uint)i, b = a + 1;
    return lerp(fog_table[a >> 2][a & 3], fog_table[b >> 2][b & 3], t);
}

float4 ps_main(PsIn i) : SV_Target
{
    if (i.fade < 255 && !fade_draws(i.fade, uint2(i.pos.xy)))
        discard;
    Mat m = mats[i.mat];
    float4 t = tex.Sample(samp, float3(i.uv, i.slice));
    // a dissolve between two steps' pictures (FrameData::dynamic_dissolve): (1 - t) of the
    // earlier one's texels and t of the later one's, alpha weighted (nothing where the object
    // is not there at a step)
    if (i.dslv.x >= 0.0)
    {
        float4 a = i.slice0 == i.slice ? t : tex.Sample(samp, float3(i.uv, i.slice0));
        float wa = (1.0 - i.dslv.x) * i.dslv.y * a.a;
        float wb = i.dslv.x * i.dslv.z * t.a;
        float al = wa + wb;
        t = float4(al > 0.0 ? (a.rgb * wa + t.rgb * wb) / al : t.rgb, al);
    }
    float4 c = i.color;
    if (m.flags & F_TEXTURED)
    {
        // colour key: keyed texels have alpha 0 (baked at upload, compared after the palette)
        if ((m.flags & F_KEYED) && t.a < 0.5)
            discard;
        c = (m.flags & F_REPLACE) ? float4(t.rgb, t.a * c.a) : c * t;
    }
    // the overlays' depth pass: only their opaque texels (renderer.rs DRAW_OVERLAY_DEPTH)
    if (draw.z > 0.0 && c.a < draw.z)
        discard;
    // fog: not on the sky, nor on blends that multiply by the frame buffer (the game turns
    // it off for them: its 2 x src x dst decals, the headlights' beams at night)
    if (clip.z > 0.5 && !(m.flags & (F_SKY | F_NOFOG)))
        c.rgb = lerp(c.rgb, fog_color.rgb, fog_from_w(i.depth * clip.w));
    return c;
}

// ---- the backing test (docs/re/native/visibility.md): statics' bases counted per pixel ----

// x: the counter (0 area, 1 / 2 visible / sky against the first pass, 3 / 4 against the first
// two), y: the backing bits a static needs to be counted (BACKING_TEST1 / 2)
cbuffer TestCb : register(b3)
{
    uint4 test;
};

RWBuffer<uint> counts : register(u1);    // five per tested static

struct BaseIn
{
    float3 pos  : POSITION;     // chunk-relative, like the merged statics
    uint   slot : SLOT;         // the static instance
};

struct BasePs
{
    float4 pos : SV_Position;
    nointerpolation uint idx : INDEX;
    float2 cd  : SV_ClipDistance0;
};

BasePs vs_base(BaseIn v)
{
    uint b = backing[v.slot];
    Xform x = xforms[statics[v.slot].x];
    float4 p = float4(v.pos, 1.0);
    float3 f = float3(dot(x.r0, p), dot(x.r1, p), dot(x.r2, p)) + offset.xyz;
    float3 e = float3(dot(rot0.xyz, f), dot(rot1.xyz, f), dot(rot2.xyz, f));
    float z = e.z;
    BasePs o;
    // the world's depth (vs_main), so the test compares with what the opaque passes wrote
    o.pos = float4(e.x * proj.x + z * proj.y, e.y * proj.z + z * proj.w, offset.w, z);
    o.idx = (b >> 8) * 5 + test.x;
    o.cd = (b & test.y) != 0 ? float2(z - clip.x, clip.y - z) : float2(-1.0, -1.0);
    return o;
}

// The depth test runs first (a fragment that fails it is not counted).
[earlydepthstencil]
void ps_base(BasePs i)
{
    InterlockedAdd(counts[i.idx], 1);
}

// Moving objects (the floating detector): an instance's surfaces below its object's base top
// (world height, clipped per vertex), counted per object after test.z static slots, against
// what the opaque statics drew (no colour, no culling, like the statics' bases).
struct DynBasePs
{
    float4 pos : SV_Position;
    nointerpolation uint idx : INDEX;
    float3 cd  : SV_ClipDistance0;      // near, far, base top
};

DynBasePs vs_dyn_base(VsIn v)
{
    uint s = v.slot + v.inst;
    Xform x = xforms[s];
    uint obj = asuint(x.extra.w) >> 8;
    float4 p = float4(v.pos, 1.0);
    float3 fr = float3(dot(x.r0, p), dot(x.r1, p), dot(x.r2, p));
    float3 f = fr + offset.xyz;
    float3 e = float3(dot(rot0.xyz, f), dot(rot1.xyz, f), dot(rot2.xyz, f));
    float z = e.z;
    DynBasePs o;
    o.pos = float4(e.x * proj.x + z * proj.y, e.y * proj.z + z * proj.w, offset.w, z);
    uint k = obj > 0 ? obj - 1 : 0;
    o.idx = (test.z + k) * 5 + test.x;
    o.cd = obj > 0 && !(s & STATIC_BIT) ? float3(z - clip.x, clip.y - z, dyn_tops[k] - fr.y) : float3(-1.0, -1.0, -1.0);
    return o;
}

[earlydepthstencil]
void ps_dyn_base(DynBasePs i)
{
    InterlockedAdd(counts[i.idx], 1);
}

// ---- the game's sky and ground (View::sky): screen-aligned quads, drawn first ----

cbuffer ScreenCb : register(b1)
{
    float4 rect;        // NDC x0, y0, x1, y1 (corners: 0 = (x0, y0), 1 = (x1, y0), 2 = (x0, y1), 3 = (x1, y1))
    float4 st;          // uv at (x0, y0) and (x1, y1)
    float4 color;       // flat colour (untextured)
    uint4  smode;       // x: textured, y: layer
};

struct ScreenPs
{
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};

ScreenPs vs_screen(uint id : SV_VertexID)
{
    ScreenPs o;
    float2 k = float2(id & 1, id >> 1);
    o.pos = float4(lerp(rect.x, rect.z, k.x), lerp(rect.y, rect.w, k.y), 0.0, 1.0);
    o.uv = lerp(st.xy, st.zw, k);
    return o;
}

float4 ps_screen(ScreenPs i) : SV_Target
{
    return smode.x ? float4(tex.Sample(samp, float3(i.uv, smode.y)).rgb, 1.0) : color;
}

// ---- render scale: the same, also marking the coverage target (SV_Target1; d3d.rs) ----

struct CovOut
{
    float4 c : SV_Target0;
    float4 k : SV_Target1;
};

CovOut ps_main_cov(PsIn i)
{
    CovOut o;
    o.c = ps_main(i);
    o.k = float4(1, 1, 1, 1);
    return o;
}

CovOut ps_screen_cov(ScreenPs i)
{
    CovOut o;
    o.c = ps_screen(i);
    o.k = float4(1, 1, 1, 1);
    return o;
}
