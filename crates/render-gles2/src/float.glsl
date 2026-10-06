// IEEE float values are carried in ordinary RGBA8 textures. This keeps signed
// filter sums between passes without requiring renderable float extensions or
// quantizing each tap. ES2's highp arithmetic reconstructs the 24-bit mantissa.
float unpack_float(vec4 packed_value) {
    vec4 b=floor(packed_value*255.0+0.5);
    float exponent=mod(b.a,128.0)*2.0+floor(b.b/128.0);
    float mantissa=b.r+b.g*256.0+mod(b.b,128.0)*65536.0;
    float value;
    if(exponent==0.0) value=mantissa*exp2(-149.0);
    else value=(1.0+mantissa/8388608.0)*exp2(exponent-127.0);
    return b.a>=128.0?-value:value;
}
vec4 pack_float(float value) {
    float magnitude=abs(value);
    if(magnitude<1.0e-30) return vec4(0.0);
    float exponent=floor(log2(magnitude));
    float normalized=magnitude*exp2(-exponent);
    if(normalized<1.0) { exponent-=1.0; normalized*=2.0; }
    if(normalized>=2.0) { exponent+=1.0; normalized*=0.5; }
    float mantissa=floor((normalized-1.0)*8388608.0+0.5);
    if(mantissa>=8388608.0) { mantissa=0.0; exponent+=1.0; }
    exponent+=127.0;
    return vec4(mod(mantissa,256.0),mod(floor(mantissa/256.0),256.0),floor(mantissa/65536.0)+mod(exponent,2.0)*128.0,floor(exponent/2.0)+(value<0.0?128.0:0.0))/255.0;
}
