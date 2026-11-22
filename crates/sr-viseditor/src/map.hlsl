// The visibility editor's window (crates/sr-viseditor): the map from above, the shapes over
// it, and the panel's pictures.

// ---- the map: the track's triangles seen from above (x right, z up), higher over lower ----

cbuffer MapCb : register(b0)
{
    float4 xf;      // ndc x = pos.x * xf.x + xf.y; ndc y = pos.z * xf.z + xf.w
    float4 dep;     // x: lowest y, y: 1 / (highest - lowest); z: flat colours; w: keyed
    uint4  info;    // x: tinted (an editing camera cell), y: hovered cell + 1, z: textured
};

Texture2D       tex    : register(t0);
Texture2D<uint> states : register(t1);   // per cell (column, row): 0 not drawn, 1 drawn, 2 added, 3 hidden, 4 by the road
SamplerState    samp   : register(s0);

struct MapIn
{
    float3 pos   : POSITION;
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;
    uint   cell  : CELL0;
};

struct MapOut
{
    float4 pos   : SV_Position;
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;
    nointerpolation uint cell : CELL0;
};

MapOut vs_map(MapIn i)
{
    MapOut o;
    // depth: higher surfaces nearer (depth test GREATER_EQUAL, cleared to 0)
    o.pos = float4(i.pos.x * xf.x + xf.y, i.pos.z * xf.z + xf.w, saturate((i.pos.y - dep.x) * dep.y) * 0.98 + 0.01, 1.0);
    o.uv = i.uv;
    o.color = i.color;
    o.cell = i.cell;
    return o;
}

float4 ps_map(MapOut i) : SV_Target
{
    float4 c = i.color;
    if (info.z != 0)
    {
        float4 t = tex.Sample(samp, i.uv);
        if (dep.w > 0.5 && t.a < 0.5)
            discard;
        if (dep.z > 0.5)
        {
            // flat colours: the texture's average colour (its 1 x 1 level)
            uint w, h, levels;
            tex.GetDimensions(0, w, h, levels);
            t.rgb = tex.SampleLevel(samp, i.uv, levels - 1).rgb;
        }
        c *= t;
    }
    uint s = 255;                                                 // no camera cell: no tint
    if (info.x != 0)
        s = states.Load(int3(i.cell & 255, i.cell >> 8, 0));
    float3 rgb = c.rgb;
    float grey = dot(rgb, float3(0.30, 0.59, 0.11));
    float3 green = float3(0.25, 0.95, 0.35);
    float3 red = float3(0.95, 0.18, 0.12);
    float3 blue = float3(0.30, 0.70, 1.00);
    if (s == 1)
        rgb = lerp(rgb, green, 0.30);                             // drawn: green
    else if (s == 2)
        rgb = lerp(rgb, green, 0.55);                             // drawn, added by the user: stronger
    else if (s == 4)
        rgb = lerp(rgb, blue, 0.45);                              // drawn, added by the road extension: blue
    else if (s == 0)
        rgb = lerp(lerp(grey.xxx, rgb, 0.5) * 0.8, red, 0.32);    // not drawn: red
    else if (s == 3)
        rgb = lerp(rgb * 0.8, red, 0.58);                         // not drawn, hidden by the user: stronger
    if (i.cell + 1 == info.y)
        rgb = rgb * 1.15 + 0.10;                                  // under the mouse
    return float4(rgb, c.a);
}

// ---- shapes (window pixels, straight colour) and pictures (premultiplied) ----

cbuffer ScreenCb : register(b1)
{
    float4 screen;  // x: 2 / width, y: 2 / height
    float4 dst;     // a picture's rectangle: left, top, right, bottom (pixels)
};

struct ShapeIn
{
    float2 pos   : POSITION;
    float4 color : COLOR0;
};

struct ShapeOut
{
    float4 pos   : SV_Position;
    float4 color : COLOR0;
};

ShapeOut vs_shape(ShapeIn i)
{
    ShapeOut o;
    o.pos = float4(i.pos.x * screen.x - 1.0, 1.0 - i.pos.y * screen.y, 0.0, 1.0);
    o.color = i.color;
    return o;
}

float4 ps_shape(ShapeOut i) : SV_Target
{
    return float4(i.color.rgb * i.color.a, i.color.a);
}

Texture2D    picture   : register(t2);
SamplerState pick_samp : register(s1);

struct PicOut
{
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};

PicOut vs_picture(uint id : SV_VertexID)
{
    PicOut o;
    float2 t = float2(id & 1, id >> 1);
    float2 p = lerp(dst.xy, dst.zw, t);
    o.pos = float4(p.x * screen.x - 1.0, 1.0 - p.y * screen.y, 0.0, 1.0);
    o.uv = t;
    return o;
}

float4 ps_picture(PicOut i) : SV_Target
{
    return picture.Sample(pick_samp, i.uv);
}
