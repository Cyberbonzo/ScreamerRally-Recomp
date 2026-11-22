// Voodoo Graphics pixel pipeline, driven by Glide 2.x state.
//
// Positions arrive in the game's Glide screen space (e.g. 640x480) and are scaled to the
// render target, so geometry keeps sub-pixel precision at any resolution.
// Interpolation follows the hardware: colours are linear in screen space (Gouraud),
// texture coordinates are s/w, t/w, 1/w linear in screen space with a per-pixel divide
// (perspective-correct), and the depth value is linear in screen space.

cbuffer State : register(b0)
{
    uint4  cc;          // colour combine: function, factor, local, other
    uint4  ac;          // alpha combine: function, factor, local, other
    uint4  tc;          // texture combine: rgb function, rgb factor, alpha function, alpha factor
    uint4  flags;       // x: invert bits (cc, ac, tc rgb, tc alpha) y: textured z: alpha test func w: alpha ref
    float4 constant_color;
    float4 fog_color;   // rgb, a = fog mode (0 off, 1 iterated alpha, 2 table)
    float4 chroma;      // rgb 0..255, w = enabled
    float4 tex_range;   // s range, t range, w-depth scale, depth bias
    float4 screen;      // 2/view width, -2/height, w-buffer (1) or z-buffer (0), x offset (widescreen)
    float4 fog_table[16];
    float4 grow;        // x: grow triangles by this many render-target pixels (0 = off), yz: target size,
                        // w: depth pulled towards the camera by this many world units (decals)
};

struct VsIn
{
    float2 pos   : POSITION;
    float  ooz   : OOZ;
    float  oow   : OOW;
    float4 color : COLOR;
    float3 tex   : TEXCOORD;
};

struct PsIn
{
    float4 pos : SV_Position;
    noperspective float4 color : COLOR;
    noperspective float3 tex   : TEXCOORD0;
    noperspective float  oow   : TEXCOORD1;
};

PsIn vs_main(VsIn v)
{
    PsIn o;
    float x = (v.pos.x + screen.w) * screen.x - 1.0;
    float y = 1.0 + v.pos.y * screen.y;
    // larger depth = nearer in both modes (reversed for precision)
    float z = screen.z > 0.5 ? v.oow * tex_range.z : v.ooz * (1.0 / 65535.0);
    // decals (shadows, smoke, tyre marks): tested as if this much nearer to the camera, so
    // the ground they lie on (bumps, the slope of the road) does not cut into them
    if (grow.w > 0.0 && screen.z > 0.5 && v.oow > 0.0)
    {
        float w = 1.0 / v.oow;
        z = tex_range.z / max(w - grow.w, w * 0.25);
    }
    o.pos = float4(x, y, saturate(z + tex_range.w), 1.0);
    o.color = v.color * (1.0 / 255.0);
    o.tex = v.tex;
    o.oow = v.oow;
    return o;
}

// Opaque 3D polygons are grown by a pixel or two (cbuffer `grow`) so that the hairline cracks
// between neighbouring polygons whose edges do not meet (T-junctions in the 1997 track
// meshes; invisible at 640 x 480, lines at 4K) close. All attributes (colour, s/w, t/w, 1/w,
// depth) are linear in screen space, so the moved corners take the values of the
// triangle's planes there: the texture mapping does not change.
[maxvertexcount(3)]
void gs_grow(triangle PsIn v[3], inout TriangleStream<PsIn> outs)
{
    if (grow.x <= 0.0)
    {
        outs.Append(v[0]); outs.Append(v[1]); outs.Append(v[2]);
        return;
    }
    float2 half_size = grow.yz * 0.5;
    float2 p[3];
    [unroll] for (int i = 0; i < 3; i++)
        p[i] = v[i].pos.xy * half_size;            // clip = NDC (w = 1): pixels
    float2 e1 = p[1] - p[0], e2 = p[2] - p[0];
    float det = e1.x * e2.y - e2.x * e1.y;
    if (abs(det) < 0.25)
    {
        outs.Append(v[0]); outs.Append(v[1]); outs.Append(v[2]);
        return;
    }
    // per-pixel gradients of every attribute
    float4 a0 = float4(v[0].color), a1 = float4(v[1].color), a2 = float4(v[2].color);
    float4 b0 = float4(v[0].tex, v[0].oow), b1 = float4(v[1].tex, v[1].oow), b2 = float4(v[2].tex, v[2].oow);
    float z0 = v[0].pos.z, z1 = v[1].pos.z, z2 = v[2].pos.z;
    float4 ag_x = ((a1 - a0) * e2.y - (a2 - a0) * e1.y) / det;
    float4 ag_y = (e1.x * (a2 - a0) - e2.x * (a1 - a0)) / det;
    float4 bg_x = ((b1 - b0) * e2.y - (b2 - b0) * e1.y) / det;
    float4 bg_y = (e1.x * (b2 - b0) - e2.x * (b1 - b0)) / det;
    float zg_x = ((z1 - z0) * e2.y - (z2 - z0) * e1.y) / det;
    float zg_y = (e1.x * (z2 - z0) - e2.x * (z1 - z0)) / det;
    [unroll] for (int k = 0; k < 3; k++)
    {
        float2 u = p[k] - p[(k + 1) % 3], w = p[k] - p[(k + 2) % 3];
        float lu = length(u), lw = length(w);
        PsIn o = v[k];
        if (lu > 1e-3 && lw > 1e-3)
        {
            float2 bis = u / lu + w / lw;
            float lb = length(bis);
            if (lb > 1e-3)
            {
                float sin_half = sqrt(max((1.0 - dot(u, w) / (lu * lw)) * 0.5, 0.0));
                float dist = min(grow.x / max(sin_half, 0.25), 4.0 * grow.x);
                float2 d = bis / lb * dist;
                o.pos.xy = (p[k] + d) / half_size;
                o.color = a0 + ag_x * (p[k].x + d.x - p[0].x) + ag_y * (p[k].y + d.y - p[0].y);
                float4 b = b0 + bg_x * (p[k].x + d.x - p[0].x) + bg_y * (p[k].y + d.y - p[0].y);
                o.tex = b.xyz;
                o.oow = max(b.w, 1e-9);
                o.pos.z = saturate(z0 + zg_x * (p[k].x + d.x - p[0].x) + zg_y * (p[k].y + d.y - p[0].y));
            }
        }
        outs.Append(o);
    }
}

Texture2D    tex0 : register(t0);
SamplerState samp : register(s0);

float select_factor(uint f, float local, float local_a, float other_a, float tex_a, float tex_c)
{
    switch (f)
    {
        case 0x0: return 0.0;
        case 0x1: return local;
        case 0x2: return other_a;
        case 0x3: return local_a;
        case 0x4: return tex_a;
        case 0x5: return tex_c;
        case 0x8: return 1.0;
        case 0x9: return 1.0 - local;
        case 0xa: return 1.0 - other_a;
        case 0xb: return 1.0 - local_a;
        case 0xc: return 1.0 - tex_a;
        case 0xd: return 1.0 - tex_c;
        default:  return 1.0;
    }
}

float apply_function(uint fn, float factor, float local, float other, float local_a)
{
    switch (fn)
    {
        case 0x0: return 0.0;
        case 0x1: return local;
        case 0x2: return local_a;
        case 0x3: return factor * other;
        case 0x4: return factor * other + local;
        case 0x5: return factor * other + local_a;
        case 0x6: return factor * (other - local);
        case 0x7: return factor * (other - local) + local;
        case 0x8: return factor * (other - local) + local_a;
        case 0x9: return -factor * local + local;
        case 0x10: return -factor * local + local_a;
        default:  return local;
    }
}

float3 combine3(uint fn, uint fac, float3 local, float3 other, float local_a, float other_a, float tex_a, float3 tex_c, bool invert)
{
    float3 r;
    [unroll] for (int i = 0; i < 3; i++)
    {
        float f = select_factor(fac, local[i], local_a, other_a, tex_a, tex_c[i]);
        r[i] = apply_function(fn, f, local[i], other[i], local_a);
    }
    r = saturate(r);
    return invert ? 1.0 - r : r;
}

float combine1(uint fn, uint fac, float local, float other, float tex_a, bool invert)
{
    float f = select_factor(fac, local, local, other, tex_a, tex_a);
    float r = saturate(apply_function(fn, f, local, other, local));
    return invert ? 1.0 - r : r;
}

float fog_from_w(float w)
{
    // Glide fog table: entry i is at w = 2^(3 + i/4) / (8 - i%4), i.e. 1 .. 52428
    w = clamp(w, 1.0, 52428.0);
    float e = floor(log2(w));
    float m = w / exp2(e);              // 1 .. 2
    float k = m >= 1.6 ? 3.0 : m >= 1.3333 ? 2.0 : m >= 1.142857 ? 1.0 : 0.0;
    float i = min(e * 4.0 + k, 62.0);
    float wi = exp2(3.0 + floor(i / 4.0)) / (8.0 - fmod(i, 4.0));
    float wj = exp2(3.0 + floor((i + 1.0) / 4.0)) / (8.0 - fmod(i + 1.0, 4.0));
    float t = saturate((w - wi) / (wj - wi));
    uint a = (uint)i, b = a + 1;
    float fa = fog_table[a >> 2][a & 3];
    float fb = fog_table[b >> 2][b & 3];
    return lerp(fa, fb, t);
}

bool alpha_test(uint fn, float a, float refv)
{
    float v = round(a * 255.0);
    switch (fn)
    {
        case 0: return false;
        case 1: return v < refv;
        case 2: return v == refv;
        case 3: return v <= refv;
        case 4: return v > refv;
        case 5: return v != refv;
        case 6: return v >= refv;
        default: return true;
    }
}

float4 ps_main(PsIn i) : SV_Target
{
    float4 iter = saturate(i.color);
    float4 texel = float4(0, 0, 0, 0);
    if (flags.y != 0)
    {
        float2 st = i.tex.xy / i.tex.z;
        float4 t = tex0.Sample(samp, st / tex_range.xy);
        // TMU0 texture combine; with one TMU the upstream ("other") input is zero.
        texel.rgb = combine3(tc.x, tc.y, t.rgb, float3(0, 0, 0), t.a, 0.0, t.a, t.rgb, (flags.x & 4) != 0);
        texel.a = combine1(tc.z, tc.w, t.a, 0.0, t.a, (flags.x & 8) != 0);
    }

    // sources of the colour and alpha combine units
    float3 c_local = cc.z == 1 ? constant_color.rgb : iter.rgb;
    float3 c_other = cc.w == 1 ? texel.rgb : cc.w == 2 ? constant_color.rgb : iter.rgb;
    float  a_local = ac.z == 1 ? constant_color.a : ac.z == 2 ? 0.0 : iter.a;
    float  a_other = ac.w == 1 ? texel.a : ac.w == 2 ? constant_color.a : iter.a;

    if (chroma.w > 0.5 && all(round(c_other * 255.0) == chroma.rgb))
        discard;

    float4 o;
    o.rgb = combine3(cc.x, cc.y, c_local, c_other, a_local, a_other, texel.a, texel.rgb, (flags.x & 1) != 0);
    o.a = combine1(ac.x, ac.y, a_local, a_other, texel.a, (flags.x & 2) != 0);

    if (!alpha_test(flags.z, o.a, (float)flags.w))
        discard;

    if (fog_color.a > 1.5)
        o.rgb = lerp(o.rgb, fog_color.rgb, fog_from_w(1.0 / max(i.oow, 1e-6)));
    else if (fog_color.a > 0.5)
        o.rgb = lerp(o.rgb, fog_color.rgb, iter.a);
    return o;
}

// ---- presentation ----

cbuffer Blit : register(b0)
{
    float4 blit;  // x: 1/gamma, y: lfb mode (1 = discard untouched pixels)
};

struct BlitOut
{
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};

BlitOut vs_blit(uint id : SV_VertexID)
{
    BlitOut o;
    o.uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

float4 ps_blit(BlitOut i) : SV_Target
{
    float4 c = tex0.Sample(samp, i.uv);
    if (blit.y > 0.5)
    {
        if (c.a < 0.5)
            discard;
        return float4(c.rgb, 1);
    }
    return float4(pow(saturate(c.rgb), blit.x), 1);
}

// ---- 2D pictures (d3d.rs `draw_picture`) ----

cbuffer Picture : register(b0)
{
    float4 pic_size;  // xy: texture size in texels, zw: target pixels per texel
    float4 pic_mode;  // x: 0 nearest, 1 sharp (pixel blocks with smoothed edges)
};

// A premultiplied picture (the frame buffer image or one of the game's pictures with a
// transparent border of one texel) over the viewport. Sharp: every texel is a flat block
// and only the last target pixel of each block blends with the next texel, by how much of
// it the next texel covers (sharp bilinear: the texel coordinate is pulled to the texel
// centre except within half a target pixel of a texel edge, then sampled bilinearly).
float4 ps_picture(BlitOut i) : SV_Target
{
    float2 t = i.uv * pic_size.xy;
    if (pic_mode.x < 0.5)
    {
        t = floor(t) + 0.5;
    }
    else
    {
        float2 scale = max(pic_size.zw, 1.0);
        float2 fl = floor(t);
        float2 d = t - fl - 0.5;
        float2 range = 0.5 - 0.5 / scale;
        t = fl + 0.5 + (d - clamp(d, -range, range)) * scale;
    }
    return tex0.SampleLevel(samp, t / pic_size.xy, 0);
}

// ---- render scale (d3d.rs, module documentation) ----

// ps_main for 3D polygons at a render scale: also marks the scene's coverage target
// (SV_Target1, 1 = a 3D polygon drew here; the blend state's write mask applies to it too).
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

Texture2D<float> cov0 : register(t1);

// The 3D layer of the scene (t0, where the coverage t1 marks it) over the full-resolution
// picture, premultiplied by its coverage (blend: one, inverse source alpha). blit.xy: scene
// pixels per picture pixel. A larger scene is averaged over each picture pixel's footprint
// with exact area weights (a box filter: every scene pixel counts, nothing aliases from
// skipped samples); a smaller one is interpolated bilinearly.
float4 ps_resolve(BlitOut i) : SV_Target
{
    uint sw, sh;
    tex0.GetDimensions(sw, sh);
    int2 last = int2(sw, sh) - 1;
    float2 k = blit.xy;
    float4 sum = float4(0, 0, 0, 0);
    if (k.x > 1.0 || k.y > 1.0)
    {
        float2 a = (i.pos.xy - 0.5) * k;
        float2 b = (i.pos.xy + 0.5) * k;
        int2 i0 = (int2)floor(a);
        [loop] for (int y = 0; y < 4; y++)
        {
            float wy = min(b.y, i0.y + y + 1.0) - max(a.y, (float)(i0.y + y));
            if (wy <= 0.0)
                continue;
            [loop] for (int x = 0; x < 4; x++)
            {
                float wx = min(b.x, i0.x + x + 1.0) - max(a.x, (float)(i0.x + x));
                if (wx <= 0.0)
                    continue;
                int3 p = int3(min(i0 + int2(x, y), last), 0);
                float c = cov0.Load(p);
                sum += float4(tex0.Load(p).rgb * c, c) * (wx * wy);
            }
        }
        return sum / (k.x * k.y);
    }
    float2 s = i.pos.xy * k - 0.5;
    int2 i0 = (int2)floor(s);
    float2 f = s - i0;
    [unroll] for (int n = 0; n < 4; n++)
    {
        int2 o = int2(n & 1, n >> 1);
        int3 p = int3(clamp(i0 + o, int2(0, 0), last), 0);
        float w = (o.x ? f.x : 1.0 - f.x) * (o.y ? f.y : 1.0 - f.y);
        float c = cov0.Load(p);
        sum += float4(tex0.Load(p).rgb * c, c) * w;
    }
    return sum;
}
