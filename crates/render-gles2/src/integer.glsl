// Little-endian bytes represent an unsigned word. Carry terms stay below
// 2^24, preserving wrapping arithmetic without ES3 integers or float FBOs.
vec4 word_add(vec4 a, vec4 b) {
    float x=a.x+b.x;
    float y=a.y+b.y+floor(x/256.0);
    float z=a.z+b.z+floor(y/256.0);
    float w=a.w+b.w+floor(z/256.0);
    return mod(vec4(x,y,z,w),256.0);
}
vec4 word_negate(vec4 a) { return word_add(vec4(255.0)-a,vec4(1.0,0.0,0.0,0.0)); }
vec4 word_subtract(vec4 a,vec4 b) { return word_add(a,word_negate(b)); }
vec4 word_multiply(vec4 a,vec4 b) {
    float x=a.x*b.x;
    float y=a.x*b.y+a.y*b.x+floor(x/256.0);
    float z=a.x*b.z+a.y*b.y+a.z*b.x+floor(y/256.0);
    float w=a.x*b.w+a.y*b.z+a.z*b.y+a.w*b.x+floor(z/256.0);
    return mod(vec4(x,y,z,w),256.0);
}
vec4 word_unsigned(float value) {
    return mod(floor(value/vec4(1.0,256.0,65536.0,16777216.0)),256.0);
}
vec4 word_from_signed(float value) {
    if(value>=2147483648.0) return vec4(255.0,255.0,255.0,127.0);
    if(value<=-2147483648.0) return vec4(0.0,0.0,0.0,128.0);
    vec4 result=word_unsigned(floor(abs(value)));
    return value<0.0?word_negate(result):result;
}
float word_unsigned_value(vec4 a) { return (a.x+a.y*256.0)+(a.z+a.w*256.0)*65536.0; }
float word_high_signed(vec4 a) { return a.z+(a.w>=128.0?a.w-256.0:a.w)*256.0; }
float word_signed_value(vec4 a) { return word_high_signed(a)*65536.0+(a.x+a.y*256.0); }
bool word_signed_less(vec4 a,vec4 b) {
    float ah=word_high_signed(a),bh=word_high_signed(b);
    return ah<bh || (ah==bh && a.x+a.y*256.0<b.x+b.y*256.0);
}
vec4 word_asr_one(vec4 a) {
    return floor(a/2.0)+vec4(mod(a.yzw,2.0)*128.0,a.w>=128.0?128.0:0.0);
}
float word_asr_eight(vec4 a) { return word_high_signed(a)*256.0+a.y; }
float word_asr_fourteen(vec4 a) { return word_high_signed(a)*4.0+floor((a.x+a.y*256.0)/16384.0); }
float word_asr_fifteen(vec4 a) { return word_high_signed(a)*2.0+floor((a.x+a.y*256.0)/32768.0); }
float word_shr_twenty_two(vec4 a) { return floor((a.z+a.w*256.0)/64.0); }
