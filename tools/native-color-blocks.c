/* Offline DXT colour re-fit for migrate-vita-native-color.py. No runtime use.
 * The target is the exact inverse-LUT of decoded old DXT. Alpha bytes never
 * enter this output: the caller preserves the original DXT5 alpha blocks. */
#include <math.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
static unsigned rd16(const uint8_t *p) { return p[0] | (p[1] << 8); }
static void wr16(uint8_t *p, unsigned x) { p[0]=x; p[1]=x>>8; }
static void rgb565(unsigned x, float c[3]) {
 unsigned r=(x>>11)&31,g=(x>>5)&63,b=x&31;
 c[0]=(r<<3)|(r>>2); c[1]=(g<<2)|(g>>4); c[2]=(b<<3)|(b>>2);
}
static unsigned quant565(const float c[3]) {
 unsigned v[3];
 for(unsigned k=0;k<3;k++) { float x=fminf(255,fmaxf(0,c[k]));v[k]=(unsigned)lroundf(x); }
 return (((v[0]*31+127)/255)<<11)|(((v[1]*63+127)/255)<<5)|((v[2]*31+127)/255);
}
struct fit { unsigned e[2]; uint32_t indices; double error; };
static struct fit candidate(unsigned e0,unsigned e1,const uint8_t pix[64],int bpe) {
 struct fit out={{e0,e1},0,0};
 if(bpe==8 && e0<e1) { out.e[0]=e1;out.e[1]=e0; }
 float p[4][3];rgb565(out.e[0],p[0]);rgb565(out.e[1],p[1]);
 for(unsigned c=0;c<3;c++) {
  p[2][c]=floorf((2*p[0][c]+p[1][c])/3);
  p[3][c]=floorf((p[0][c]+2*p[1][c])/3);
  if(bpe==8 && out.e[0]<=out.e[1])p[2][c]=floorf((p[0][c]+p[1][c])/2);
 }
 unsigned choices=(bpe==8 && out.e[0]<=out.e[1])?3:4;
 unsigned alpha_total=0;for(unsigned i=0;i<16;i++)alpha_total+=pix[i*4+3];
 for(unsigned i=0;i<16;i++) {
  double best=1e30;unsigned index=0;
  for(unsigned j=0;j<choices;j++) {
   double error=0;for(unsigned c=0;c<3;c++){double d=pix[i*4+c]-p[j][c];error+=d*d;}
   if(error<best){best=error;index=j;}
  }
  out.indices|=index<<(2*i);
  unsigned alpha=alpha_total?pix[i*4+3]:255;out.error+=best*alpha*alpha;
 }
 return out;
}
size_t native_color_refine(uint8_t *linear,const uint8_t *pixels,const uint8_t *old,
 size_t count,unsigned bpe,unsigned old_bpe,const uint8_t inverse[256]) {
 size_t changed=0;unsigned offset=bpe==8?0:8,old_offset=old_bpe==8?0:8;
 for(size_t block=0;block<count;block++) {
  uint8_t *dst=linear+block*bpe+offset;const uint8_t *pix=pixels+block*64;
  struct fit best=candidate(rd16(dst),rd16(dst+2),pix,bpe);
  float c0[3],c1[3];rgb565(rd16(old+block*old_bpe+old_offset),c0);rgb565(rd16(old+block*old_bpe+old_offset+2),c1);
  for(unsigned c=0;c<3;c++){c0[c]=inverse[(unsigned)c0[c]];c1[c]=inverse[(unsigned)c1[c]];}
  struct fit trial=candidate(quant565(c0),quant565(c1),pix,bpe);
  if(trial.error<best.error)best=trial;
  for(unsigned iteration=0;iteration<2;iteration++) {
   double aa=0,bb=0,ab=0,ax[3]={0},bx[3]={0};unsigned alpha_total=0;
   for(unsigned i=0;i<16;i++)alpha_total+=pix[i*4+3];
   for(unsigned i=0;i<16;i++) {
    unsigned ix=(best.indices>>(2*i))&3;double a=ix==0?1:ix==1?0:ix==2?2.0/3:1.0/3;
    if(bpe==8&&best.e[0]<=best.e[1]&&ix==2)a=.5;
    double b=1-a,alpha=alpha_total?pix[i*4+3]:255,w=alpha*alpha;
    aa+=w*a*a;bb+=w*b*b;ab+=w*a*b;
    for(unsigned c=0;c<3;c++){ax[c]+=w*a*pix[i*4+c];bx[c]+=w*b*pix[i*4+c];}
   }
   double determinant=aa*bb-ab*ab;
   if(determinant<=1e-6)break;
   for(unsigned c=0;c<3;c++){c0[c]=(ax[c]*bb-bx[c]*ab)/determinant;c1[c]=(bx[c]*aa-ax[c]*ab)/determinant;}
   trial=candidate(quant565(c0),quant565(c1),pix,bpe);
   if(trial.error<best.error)best=trial;else break;
  }
  uint8_t result[8];wr16(result,best.e[0]);wr16(result+2,best.e[1]);
  for(unsigned i=0;i<4;i++)result[4+i]=best.indices>>(8*i);
  if(memcmp(dst,result,8)){memcpy(dst,result,8);changed++;}
 }
 return changed;
}
