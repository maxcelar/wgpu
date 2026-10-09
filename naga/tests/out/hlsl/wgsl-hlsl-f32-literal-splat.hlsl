struct U {
    float4 a;
    float4 b;
};

cbuffer u : register(b0) { U u; }

float4 main() : SV_Target0
{
    float4 _e2 = u.b;
    float _e7 = u.b.x;
    float3 phi = ((_e2.yzw < (_e7).xxx) ? (3.1415927f).xxx : (0.0f).xxx);
    float _e18 = u.a.x;
    return float4(cos(((_e18 * float3(1681000.0f, 1795300.0f, 2208400.0f)) + phi)), 1.0f);
}
