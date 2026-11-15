//! 常驻组件按实际占位排布，并在开关变化时连续补位。
use crate::anim::{smoothstep, Spring};

pub(crate) const CLOCK: usize = 0;
pub(crate) const BATTERY: usize = 1;
pub(crate) const QUOTA: usize = 2;
/// 紧跟时钟的天气徽标；不参与用户排序，永远与时钟成组。
pub(crate) const WEATHER: usize = 5;
pub(crate) fn quota_slot(position:usize)->usize {
    if position<3 {QUOTA+position} else {WIDTHS.len()+position-3}
}
pub(crate) const MUSIC: usize = 6;
pub(crate) const FOCUS: usize = 7;
pub(crate) const BELL: usize = 8;
pub(crate) const PET: usize = 9;
/// 天气徽标的基础槽宽；温度文字更长时由 `widen` 补足。
pub(crate) const WEATHER_WIDTH: f32 = 38.0;
const WIDTHS: [f32; 10] = [44.0,20.0,27.0,27.0,27.0,WEATHER_WIDTH,14.0,crate::focus_bar::WIDTH,20.0,32.0];

pub(crate) const DEFAULT_ORDER: [usize;6]=[BATTERY,PET,CLOCK,QUOTA,MUSIC,FOCUS];
pub(crate) fn valid_order(order:&[usize])->bool {
    order.len()==6 && DEFAULT_ORDER.iter().all(|slot|order.iter().filter(|value|*value==slot).count()==1)
}

pub(crate) fn animations_enabled()->bool {
    #[cfg(windows)] {
        static ENABLED:std::sync::OnceLock<bool>=std::sync::OnceLock::new();
        *ENABLED.get_or_init(||{
            use windows::Win32::{Foundation::BOOL,UI::WindowsAndMessaging::*};
            let mut enabled=BOOL(1);
            unsafe {let _=SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION,0,Some((&mut enabled as *mut BOOL).cast()),SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));}
            enabled.as_bool()
        })
    }
    #[cfg(not(windows))] {true}
}

#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub width: f32,
    pub x: Vec<Option<f32>>,
    pub widths:Vec<f32>,
    quotas:usize,
    /// macOS 刘海机型：常驻行整体下移的距离（0 = 无刘海，Windows 永远是 0）。
    pub notch_y: f32,
}

impl Layout {
    pub fn new(battery:bool,quotas:usize,music:bool,focus:bool,bell:bool,pet:bool,weather:bool)->Self {
        fn place(cursor:&mut f32,width:f32)->f32 {let x=*cursor+width/2.0;*cursor+=width+12.0;x}
        let mut widths=WIDTHS.to_vec(); widths.resize(WIDTHS.len()+quotas.saturating_sub(3),27.0);
        let mut x=vec![None;widths.len()];let mut cursor=16.0;
        let count=quotas;
        let order=if battery {[BATTERY,CLOCK,QUOTA]}else{[QUOTA,CLOCK,BATTERY]};
        for slot in order {
            match slot {
                BATTERY if battery=>x[slot]=Some(place(&mut cursor,WIDTHS[slot])),
                CLOCK=>{
                    if pet {x[PET]=Some(place(&mut cursor,WIDTHS[PET]));}
                    x[slot]=Some(place(&mut cursor,WIDTHS[slot]));
                    // 天气紧跟时钟，中间只留组件间距；排序时也与时钟成组移动。
                    if weather {x[WEATHER]=Some(place(&mut cursor,WIDTHS[WEATHER]));}
                },
                QUOTA if count>0=>{
                    let width=count as f32*27.0+(count-1) as f32*7.0;
                    let left=place(&mut cursor,width)-width/2.0;
                    for i in 0..count {x[quota_slot(i)]=Some(left+13.5+i as f32*34.0);}
                },
                _=>{},
            }
        }
        for (slot,on) in [(MUSIC,music&&!bell),(FOCUS,focus),(BELL,bell)] {
            if on {x[slot]=Some(place(&mut cursor,WIDTHS[slot]));}
        }
        let width=cursor-12.0+16.0;
        for value in x.iter_mut().flatten() {*value-=width/2.0;}
        Self {width:width.max(96.0),x,widths,quotas,notch_y:0.0}
    }
    pub fn quota_slots(&self)->impl Iterator<Item=usize> { (0..self.quotas).map(quota_slot) }
    /// 把某个槽加宽 extra：其余槽对称让位并记入总宽。点数文字、时段时钟、天气温度共用。
    pub fn widen(&mut self,slot:usize,extra:f32){
        if extra<=0.0 {return;}
        let Some(center)=self.x.get(slot).copied().flatten() else {return;};
        for (i,x) in self.x.iter_mut().enumerate(){if i!=slot{if let Some(x)=x{*x+=if *x<center{-extra/2.0}else{extra/2.0};}}}
        self.widths[slot]+=extra;self.width+=extra;
    }
    pub fn balance_tail(&mut self,position:usize,extra:f32){self.widen(quota_slot(position),extra);}
    pub fn widen_clock(&mut self, extra:f32) {self.widen(CLOCK,extra);}
    /// **macOS 专用**（刘海与胶囊两种岛形共用）：常驻行按固定槽位左右分列——
    /// **左组＝电源 + 吉祥宠 + 天气；右组＝AI 额度环 + 音乐 + 专注 + 铃铛**。
    /// 时钟隐藏（macOS 右上角系统菜单栏本来就有时间，岛里不重复显示）。
    ///
    /// 左组整体右对齐到空洞左缘，天气排在最右（紧贴刘海）；新增组件时
    /// 电源/宠物自动向岛体左缘方向让位。右组只挂额度环等状态件。
    ///
    /// 有刘海机型在中间留出物理刘海空洞（gap=刘海宽+呼吸位）；胶囊模式
    /// gap=0，两组之间用固定视觉间距替代。弹窗类（歌词 / 充放电 / 实时状态）
    /// 仍在岛体下方延伸弹出，不受本布局影响。
    pub fn notch_slots(&mut self, gap: f32) {
        // 时间交给系统菜单栏，时钟槽位撤掉；天气槽位独立保留（原与时钟成组）。
        self.x[CLOCK] = None;
        const PAD: f32 = 14.0;   // 空洞边缘到第一个图标
        const EDGE: f32 = 16.0;  // 图标组到岛体圆角
        const SPACE: f32 = 12.0; // 组件间距，与 Layout::new 一致
        // 胶囊模式没有物理刘海，两组之间留固定视觉间距。
        let hole = if gap > 0.0 { gap } else { 48.0 };
        let left: Vec<usize> = [BATTERY, PET, WEATHER].iter().copied().filter(|&i| self.x[i].is_some()).collect();
        let right: Vec<usize> = self.quota_slots()
            .chain([MUSIC, FOCUS, BELL])
            .filter(|&i| self.x[i].is_some()).collect();
        if left.is_empty() && right.is_empty() { return; }
        let group_width = |slots: &[usize]| -> f32 {
            slots.iter().map(|&i| self.widths[i]).sum::<f32>()
                + SPACE * slots.len().saturating_sub(1) as f32
        };
        let left_w = group_width(&left);
        let right_w = group_width(&right);
        let half = hole / 2.0;
        // 左组右对齐到空洞左缘，按 [电源, 吉祥宠, 天气] 顺序向左排。
        let mut cursor = -half - PAD - left_w;
        for &i in &left {
            self.x[i] = Some(cursor + self.widths[i] / 2.0);
            cursor += self.widths[i] + SPACE;
        }
        // 右组左对齐到空洞右缘，按 [额度环…, 音乐, 专注, 铃铛] 顺序向右排。
        let mut cursor = half + PAD;
        for &i in &right {
            self.x[i] = Some(cursor + self.widths[i] / 2.0);
            cursor += self.widths[i] + SPACE;
        }
        self.width = (hole + 2.0 * (PAD + EDGE) + left_w + right_w).max(96.0);
        // 常驻行保持在菜单栏高度内，不再下挂（下挂方案已废弃）。
        self.notch_y = 0.0;
    }
    pub fn reorder(&mut self,order:&[usize]) {
        if !valid_order(order){return;}
        let original=self.x.clone();let mut cursor=-self.width/2.0+16.0;
        for slot in order.iter().copied().chain(std::iter::once(BELL)) {
            let slots:Vec<usize>=if slot==QUOTA{self.quota_slots().collect()}
                else if slot==CLOCK&&self.x[WEATHER].is_some(){vec![CLOCK,WEATHER]}
                else{vec![slot]};
            let left=slots.iter().filter_map(|&i|original[i].map(|x|x-self.widths[i]/2.0)).fold(f32::INFINITY,f32::min);
            let right=slots.iter().filter_map(|&i|original[i].map(|x|x+self.widths[i]/2.0)).fold(f32::NEG_INFINITY,f32::max);
            if !left.is_finite(){continue;}
            for i in slots {if let Some(x)=original[i]{self.x[i]=Some(cursor+x-left);}}
            cursor+=right-left+12.0;
        }
    }
    /// 圆环换边经过时钟时，暂时淡出时钟，避免移动中相互遮挡。
    pub fn clearance(&self,slot:usize)->f32 {
        let Some(x)=self.x[slot] else{return 0.0;};
        let distance=self.quota_slots().filter_map(|i|self.x[i].map(|other|(other-x).abs()-(self.widths[i]-27.0)/2.0)).fold(f32::INFINITY,f32::min);
        let quota=if slot==CLOCK {smoothstep(34.0,44.0,distance)}else{smoothstep(22.0,34.0,distance)};
        quota*self.pet_clearance(slot)
    }
    pub fn pet_clearance(&self,slot:usize)->f32 {
        match (self.x[slot],self.x[PET]) {
            (Some(x),Some(pet))=>smoothstep(self.widths[slot]/2.0+14.0,self.widths[slot]/2.0+24.0,(x-pet).abs()),
            _=>1.0,
        }
    }
}

pub(crate) struct Motion {
    pub target: Layout,
    positions: Vec<Spring>,
    initialized: bool,
}
impl Default for Motion {
    fn default()->Self {Self {target:Layout::new(false,0,false,false,false,false,false),positions:vec![Spring::new(0.0,360.0,0.76);10],initialized:false}}
}
impl Motion {
    pub fn step(&mut self,target:Layout,dt:f32,animate:bool) {
        self.positions.resize(target.x.len(),Spring::new(0.0,360.0,0.76));
        for (i,x) in target.x.iter().enumerate() {
            if let Some(x)=x {
                let spring=&mut self.positions[i];spring.target=*x;
                if !self.initialized || self.target.x.get(i).copied().flatten().is_none() || !animate {spring.snap(*x);}else{spring.step(dt);}
            }
        }
        self.target=target;self.initialized=true;
    }
    pub fn current(&self)->Layout {
        let mut layout=self.target.clone();
        for (i,x) in layout.x.iter_mut().enumerate() {if x.is_some(){*x=Some(self.positions[i].pos);}}
        layout
    }
    pub fn position(&self,slot:usize)->f32 {self.positions[slot].pos}
    pub fn settled(&self)->bool {self.target.x.iter().enumerate().all(|(i,x)|x.is_none()||self.positions[i].settled(100.0))}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn deepseek_balance_follows_more_and_never_overlaps(){
        for order in [DEFAULT_ORDER,[FOCUS,MUSIC,QUOTA,CLOCK,PET,BATTERY]] {for quotas in 1..=6 {for battery in [false,true]{
            let mut layout=Layout::new(battery,quotas,true,true,false,true,false);layout.reorder(&order);layout.balance_tail(quotas-1,76.0);layout.widen_clock(40.0);
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|{let width=layout.widths[i];(x-width/2.0,x+width/2.0)})).collect();boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
            assert!(layout.width<crate::app::WIN_W);
        }}}
    }
    #[test]
    fn clock_12h_keeps_components_separate_in_custom_orders(){
        for order in [DEFAULT_ORDER,[FOCUS,MUSIC,QUOTA,CLOCK,PET,BATTERY]] {
            for quotas in 0..=5 {
                let mut layout=Layout::new(true,quotas,true,true,false,true,false);
                layout.reorder(&order);layout.widen_clock(40.0);
                let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|{
                    let width=layout.widths[i];
                    (x-width/2.0,x+width/2.0)
                })).collect();
                boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
                assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
                assert!(boxes.first().unwrap().0>=-layout.width/2.0+15.9);
                assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
            }
        }
    }
    #[test]
    fn custom_order_preserves_groups_and_hidden_positions(){
        assert!(!valid_order(&[0,0,2,6,7,9]));
        assert!(!valid_order(&[0,1,2]));
        let order=[FOCUS,MUSIC,QUOTA,CLOCK,PET,BATTERY];
        for battery in [false,true]{for count in 0..=5 {
            let mut layout=Layout::new(battery,count,true,true,false,true,false);
            let width=layout.width;layout.reorder(&order);
            assert_eq!(layout.width,width);
            let positions:Vec<_>=order.iter().filter_map(|i|layout.x[*i]).collect();
            assert!(positions.windows(2).all(|p|p[0]<p[1]));
            if count>1 {assert_eq!(layout.x[QUOTA+1].unwrap()-layout.x[QUOTA].unwrap(),34.0);}
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
            boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
        }}
    }
    #[test]
    fn every_component_combination_fits_without_overlaps(){
        for quotas in 0..=8 {for battery in [false,true] {for music in [false,true] {for focus in [false,true] {for bell in [false,true] {for pet in [false,true] {for weather in [false,true] {
            let layout=Layout::new(battery,quotas,music,focus,bell,pet,weather);
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
            boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert_eq!(layout.quota_slots().count(),quotas);
            assert!(boxes.first().unwrap().0>=-layout.width/2.0+15.9);
            assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
            assert!(boxes.windows(2).all(|pair|pair[1].0-pair[0].1>=6.9),"组件不能挤在一起：{layout:?}");
            assert_eq!(layout.clearance(CLOCK),1.0);
        }}}}}}}
    }
    #[test]
    fn weather_sits_after_the_clock_and_follows_it_in_custom_orders(){
        for order in [DEFAULT_ORDER,[FOCUS,MUSIC,QUOTA,CLOCK,PET,BATTERY],[QUOTA,MUSIC,CLOCK,PET,FOCUS,BATTERY]] {
            for quotas in 0..=3 {for battery in [false,true] {
                let mut layout=Layout::new(battery,quotas,true,true,false,true,true);
                layout.reorder(&order);
                let (clock,weather)=match (layout.x[CLOCK],layout.x[WEATHER]) {(Some(c),Some(w))=>(c,w),_=>panic!("时钟与天气都应就位：{layout:?}")};
                assert!(weather>clock+22.0+6.9,"天气应紧跟时钟右侧：时钟 {clock}，天气 {weather}");
                // reorder 后时钟与天气的相对距离保持成组前的间隔。
                assert!((weather-clock-(38.0+44.0)/2.0-12.0).abs()<0.01);
                let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
                boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
                assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
                assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
                assert!(layout.clearance(WEATHER)>0.99,"天气与额度环不应对撞：{layout:?}");
            }}
        }
    }
    #[test]
    fn weather_chip_widens_symmetrically_without_overlaps(){
        let mut layout=Layout::new(true,3,true,true,false,true,true);
        let before=layout.x.clone();
        layout.widen(WEATHER,18.0);
        assert!((layout.widths[WEATHER]-56.0).abs()<0.01);
        for (slot,x) in layout.x.iter().enumerate() {
            match (slot,x) {
                (crate::bar_layout::WEATHER,Some(_))=>{},
                (_,Some(x))=>assert!((*x-before[slot].unwrap()).abs()>8.0,"其余槽位应被推开：{slot}"),
                (_,None)=>{},
            }
        }
        let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
        boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
        assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
        // 未就位的槽位调用 widen 应是空操作。
        let mut empty=Layout::new(true,0,true,true,false,true,false);
        let width=empty.width;empty.widen(WEATHER,20.0);
        assert_eq!(empty.width,width);
    }
    #[test]
    fn notch_layout_keeps_weather_left_and_quotas_right_of_the_hole(){
        let mut layout=Layout::new(true,2,true,false,false,true,true);
        layout.notch_slots(200.0);
        let get=|i:usize|layout.x[i].unwrap();
        // 左组 [电源, 宠物, 天气] 整体在刘海空洞（±100）左侧，天气最靠右（紧贴空洞）。
        assert!(get(BATTERY)<get(PET)&&get(PET)<get(WEATHER)&&get(WEATHER)< -100.0-13.0);
        // 右组从额度环开始，天气不再出现在右侧。
        let quotas:Vec<_>=layout.quota_slots().map(get).collect();
        assert!(quotas.iter().all(|&x|x>100.0+13.0));
        assert!(quotas[0]<quotas[1]);
        // 天气右缘到空洞中心留出 PAD+半径的呼吸位，不越过空洞。
        assert!(get(WEATHER)+38.0/2.0<=-100.0-14.0+0.01);
        // 岛体总宽按两组实占计算，不小于空洞与边距之和。
        assert!(layout.width>=200.0+2.0*(14.0+16.0));
    }
    #[test]
    fn replacing_battery_moves_continuously_and_supports_an_instant_state(){
        let before=Layout::new(true,1,true,false,false,true,false);let after=Layout::new(false,1,true,false,false,true,false);
        let mut motion=Motion::default();motion.step(before.clone(),0.0,true);
        motion.step(after.clone(),1.0/60.0,true);let mid=motion.current().x[QUOTA].unwrap();
        assert!(mid<before.x[QUOTA].unwrap()&&mid>after.x[QUOTA].unwrap());
        for _ in 0..120 {motion.step(after.clone(),1.0/60.0,true);}
        assert!((motion.current().x[QUOTA].unwrap()-after.x[QUOTA].unwrap()).abs()<0.01);
        motion.step(before.clone(),1.0/60.0,false);assert_eq!(motion.current().x,before.x);
    }
}
